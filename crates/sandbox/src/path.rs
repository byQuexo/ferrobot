use std::fmt;

use crate::SandboxError;

/// An absolute, normalized path inside a sandbox. Rejects relative paths, `.`/`..`, empty
/// components and NUL bytes, so it maps safely onto a host directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxPath(String);

impl SandboxPath {
    pub fn parse(raw: &str) -> Result<Self, SandboxError> {
        let invalid = |reason| SandboxError::InvalidPath {
            path: raw.to_owned(),
            reason,
        };
        if !raw.starts_with('/') {
            return Err(invalid("must be absolute"));
        }
        if raw.contains('\0') {
            return Err(invalid("contains NUL"));
        }
        if raw == "/" {
            return Err(invalid("must name a file"));
        }
        let trimmed = raw.trim_end_matches('/');
        for part in trimmed[1..].split('/') {
            match part {
                "" => return Err(invalid("empty path component")),
                "." | ".." => return Err(invalid("'.' and '..' are not allowed")),
                _ => {}
            }
        }
        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parent directory (`/` for top-level entries).
    pub fn parent(&self) -> &str {
        match self.0.rfind('/') {
            Some(0) | None => "/",
            Some(i) => &self.0[..i],
        }
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or_default()
    }

    /// Path relative to `/`, for joining onto a host directory.
    pub fn relative(&self) -> &str {
        &self.0[1..]
    }
}

impl fmt::Display for SandboxPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_splits() {
        let p = SandboxPath::parse("/home/node/notes.txt").unwrap();
        assert_eq!(p.parent(), "/home/node");
        assert_eq!(p.file_name(), "notes.txt");
        assert_eq!(p.relative(), "home/node/notes.txt");
        assert_eq!(SandboxPath::parse("/a").unwrap().parent(), "/");
        assert_eq!(SandboxPath::parse("/a/b/").unwrap().as_str(), "/a/b");
    }

    #[test]
    fn rejects_escapes() {
        for bad in ["", "rel/x", "/", "/a/../etc", "/a/./b", "/a//b", "/a\0b"] {
            assert!(SandboxPath::parse(bad).is_err(), "{bad:?}");
        }
    }
}
