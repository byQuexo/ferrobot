//! Encryption at rest for archives in object storage: AES-256-GCM over 64 KiB
//! chunks (the STREAM construction), so gigabyte images never sit in memory.
//!
//! Layout: `MAGIC`, a random 7-byte nonce prefix, then chunks of plaintext
//! plus a 16-byte tag. A chunk's nonce is prefix, big-endian counter and a
//! last-chunk flag, so reordered, dropped or appended chunks fail to open.
//! The object's key is the AAD: an archive copied to another path fails too.

use std::sync::Arc;

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};

use crate::SandboxError;

const MAGIC: &[u8; 8] = b"FBSEAL01";
const PREFIX: usize = 7;
const HEADER: usize = MAGIC.len() + PREFIX;
const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;

/// The key archives are sealed with (32 bytes). `Debug` never shows it.
#[derive(Clone)]
pub struct SealKey(Arc<LessSafeKey>);

impl SealKey {
    /// # Panics
    /// Never: 32 bytes are always an AES-256 key.
    pub fn new(bytes: &[u8; 32]) -> Self {
        let key = UnboundKey::new(&AES_256_GCM, bytes).expect("32 bytes is an AES-256 key");
        Self(Arc::new(LessSafeKey::new(key)))
    }
}

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey([REDACTED])")
    }
}

fn damaged() -> SandboxError {
    SandboxError::Other("archive cannot be decrypted: wrong key or damaged".into())
}

fn nonce(prefix: [u8; PREFIX], counter: u32, last: bool) -> Nonce {
    let mut n = [0u8; NONCE_LEN];
    n[..PREFIX].copy_from_slice(&prefix);
    n[PREFIX..PREFIX + 4].copy_from_slice(&counter.to_be_bytes());
    n[NONCE_LEN - 1] = u8::from(last);
    Nonce::assume_unique_for_key(n)
}

/// Encrypts one object as it streams by.
pub struct Sealer {
    key: SealKey,
    aad: Vec<u8>,
    prefix: [u8; PREFIX],
    counter: u32,
    buf: Vec<u8>,
    header: bool,
}

impl Sealer {
    /// `object`: the storage key the result is written to.
    pub fn new(key: &SealKey, object: &str) -> Result<Self, SandboxError> {
        let mut prefix = [0u8; PREFIX];
        SystemRandom::new()
            .fill(&mut prefix)
            .map_err(|_| SandboxError::Other("no randomness for a nonce".into()))?;
        Ok(Self {
            key: key.clone(),
            aad: object.as_bytes().to_vec(),
            prefix,
            counter: 0,
            buf: Vec::new(),
            header: false,
        })
    }

    fn chunk(
        &mut self,
        mut plain: Vec<u8>,
        last: bool,
        out: &mut Vec<u8>,
    ) -> Result<(), SandboxError> {
        if !self.header {
            out.extend_from_slice(MAGIC);
            out.extend_from_slice(&self.prefix);
            self.header = true;
        }
        let n = nonce(self.prefix, self.counter, last);
        self.key
            .0
            .seal_in_place_append_tag(n, Aad::from(&self.aad), &mut plain)
            .map_err(|_| damaged())?;
        out.extend_from_slice(&plain);
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| SandboxError::Other("archive too large to seal".into()))?;
        Ok(())
    }

    /// Sealed bytes ready so far. The last chunk waits for [`Self::finish`].
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, SandboxError> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while self.buf.len() > CHUNK {
            let rest = self.buf.split_off(CHUNK);
            let plain = std::mem::replace(&mut self.buf, rest);
            self.chunk(plain, false, &mut out)?;
        }
        Ok(out)
    }

    pub fn finish(mut self) -> Result<Vec<u8>, SandboxError> {
        let mut out = Vec::new();
        let plain = std::mem::take(&mut self.buf);
        self.chunk(plain, true, &mut out)?;
        Ok(out)
    }
}

enum State {
    Header(Vec<u8>),
    Sealed {
        prefix: [u8; PREFIX],
        counter: u32,
        buf: Vec<u8>,
    },
    Plain,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Sealer")
    }
}

/// Decrypts one object as it streams by.
pub struct Opener {
    key: Option<SealKey>,
    aad: Vec<u8>,
    accept_plain: bool,
    state: State,
}

impl Opener {
    /// `accept_plain`: objects without the header pass through unchanged
    /// (archives written before encryption, in a local store only).
    pub fn new(key: Option<&SealKey>, object: &str, accept_plain: bool) -> Self {
        Self {
            key: key.cloned(),
            aad: object.as_bytes().to_vec(),
            accept_plain,
            state: State::Header(Vec::new()),
        }
    }

    fn open(
        &self,
        prefix: [u8; PREFIX],
        counter: u32,
        last: bool,
        mut sealed: Vec<u8>,
    ) -> Result<Vec<u8>, SandboxError> {
        let key = self.key.as_ref().ok_or_else(|| {
            SandboxError::Other("archive is encrypted and no storage key is loaded".into())
        })?;
        let len = key
            .0
            .open_in_place(
                nonce(prefix, counter, last),
                Aad::from(&self.aad),
                &mut sealed,
            )
            .map_err(|_| damaged())?
            .len();
        sealed.truncate(len);
        Ok(sealed)
    }

    fn not_sealed() -> SandboxError {
        SandboxError::Other("archive is not encrypted; refusing it outside a local store".into())
    }

    /// Plaintext ready so far.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<u8>, SandboxError> {
        if let State::Header(head) = &mut self.state {
            head.extend_from_slice(data);
            if head.len() < HEADER && head.starts_with(&MAGIC[..head.len().min(MAGIC.len())]) {
                return Ok(Vec::new());
            }
            let head = std::mem::take(head);
            if !head.starts_with(MAGIC) {
                if !self.accept_plain {
                    return Err(Self::not_sealed());
                }
                self.state = State::Plain;
                return Ok(head);
            }
            let mut prefix = [0u8; PREFIX];
            prefix.copy_from_slice(&head[MAGIC.len()..HEADER]);
            self.state = State::Sealed {
                prefix,
                counter: 0,
                buf: Vec::new(),
            };
            return self.push(&head[HEADER..]);
        }
        let State::Sealed { buf, .. } = &mut self.state else {
            return Ok(data.to_vec());
        };
        buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let State::Sealed {
                prefix,
                counter,
                buf,
            } = &mut self.state
            else {
                return Ok(out);
            };
            // The last chunk is only known at the end: keep one full chunk back.
            if buf.len() <= CHUNK + TAG {
                return Ok(out);
            }
            let rest = buf.split_off(CHUNK + TAG);
            let sealed = std::mem::replace(buf, rest);
            let (prefix, n) = (*prefix, *counter);
            *counter = counter.checked_add(1).ok_or_else(damaged)?;
            out.extend(self.open(prefix, n, false, sealed)?);
        }
    }

    pub fn finish(mut self) -> Result<Vec<u8>, SandboxError> {
        match std::mem::replace(&mut self.state, State::Plain) {
            State::Header(head) if self.accept_plain && !head.starts_with(MAGIC) => Ok(head),
            State::Header(head) if head.starts_with(MAGIC) => Err(damaged()),
            State::Header(_) => Err(Self::not_sealed()),
            State::Sealed {
                prefix,
                counter,
                buf,
            } => self.open(prefix, counter, true, buf),
            State::Plain => Ok(Vec::new()),
        }
    }
}

impl std::fmt::Debug for Opener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Opener")
    }
}

/// Seals a whole object in memory.
pub fn seal(key: &SealKey, object: &str, data: &[u8]) -> Result<Vec<u8>, SandboxError> {
    let mut sealer = Sealer::new(key, object)?;
    let mut out = sealer.push(data)?;
    out.extend(sealer.finish()?);
    Ok(out)
}

/// Opens a whole object in memory.
pub fn open(
    key: Option<&SealKey>,
    object: &str,
    data: &[u8],
    accept_plain: bool,
) -> Result<Vec<u8>, SandboxError> {
    let mut opener = Opener::new(key, object, accept_plain);
    let mut out = opener.push(data)?;
    out.extend(opener.finish()?);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];
    const OBJ: &str = "archives/ann/home.tar";

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect()
    }

    #[test]
    fn round_trips_at_every_chunk_boundary_and_in_any_pieces() {
        let key = SealKey::new(&KEY);
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 17] {
            let plain = data(len);
            let sealed = seal(&key, OBJ, &plain).unwrap();
            assert_eq!(
                sealed.len(),
                HEADER + len + TAG * (len.div_ceil(CHUNK).max(1)),
                "{len}"
            );
            for piece in [1, 1000, CHUNK + TAG, sealed.len().max(1)] {
                let mut opener = Opener::new(Some(&key), OBJ, false);
                let mut out = Vec::new();
                for part in sealed.chunks(piece) {
                    out.extend(opener.push(part).unwrap());
                }
                out.extend(opener.finish().unwrap());
                assert_eq!(out, plain, "{len} in pieces of {piece}");
            }
        }
    }

    #[test]
    fn refuses_wrong_key_other_object_truncation_and_tampering() {
        let key = SealKey::new(&KEY);
        let plain = data(2 * CHUNK + 5);
        let sealed = seal(&key, OBJ, &plain).unwrap();
        let open_with = |key: &SealKey, obj: &str, bytes: &[u8]| open(Some(key), obj, bytes, true);
        assert!(
            open_with(&SealKey::new(&[8; 32]), OBJ, &sealed).is_err(),
            "wrong key"
        );
        assert!(
            open_with(&key, "archives/bob/home.tar", &sealed).is_err(),
            "other object"
        );
        let whole_chunks = HEADER + 2 * (CHUNK + TAG);
        assert!(
            open_with(&key, OBJ, &sealed[..whole_chunks]).is_err(),
            "last chunk dropped"
        );
        assert!(
            open_with(&key, OBJ, &sealed[..sealed.len() - 1]).is_err(),
            "cut short"
        );
        let mut longer = sealed.clone();
        longer.extend_from_slice(&sealed[HEADER..HEADER + CHUNK + TAG]);
        assert!(open_with(&key, OBJ, &longer).is_err(), "chunk appended");
        let mut flipped = sealed.clone();
        flipped[HEADER + 100] ^= 1;
        assert!(open_with(&key, OBJ, &flipped).is_err(), "byte flipped");
        assert!(open(None, OBJ, &sealed, true).is_err(), "no key");
        assert!(
            !sealed.windows(64).any(|w| w == &plain[..64]),
            "no plaintext inside"
        );
        assert_ne!(
            seal(&key, OBJ, &plain).unwrap(),
            sealed,
            "fresh nonce per write"
        );
    }

    #[test]
    fn plaintext_passes_only_where_allowed() {
        let key = SealKey::new(&KEY);
        let tar = b"home/node/notes.md\0\0\0 old archive".to_vec();
        assert_eq!(open(Some(&key), OBJ, &tar, true).unwrap(), tar);
        assert_eq!(open(None, OBJ, b"FBSE", true).unwrap(), b"FBSE");
        assert!(open(Some(&key), OBJ, &tar, false).is_err());
        assert!(open(Some(&key), OBJ, b"", false).is_err());
        assert!(
            open(Some(&key), OBJ, &MAGIC[..], true).is_err(),
            "header cut short"
        );
    }

    #[test]
    fn debug_never_shows_the_key() {
        assert_eq!(format!("{:?}", SealKey::new(&KEY)), "SealKey([REDACTED])");
    }
}
