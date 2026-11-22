//! The latest daemon log lines in memory, for `GET /v1/diagnostics/log`.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use tracing::Subscriber;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::registry::LookupSpan;

/// Most lines kept, and the most `/v1/diagnostics/log` returns.
pub const MAX_LINES: usize = 2000;
const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const REDACTED: &str = "[REDACTED]";

/// The latest log lines, without colour codes and with credentials redacted. Cheap to clone.
#[derive(Clone, Default)]
pub struct LogRing(Arc<Mutex<Ring>>);

#[derive(Default)]
struct Ring {
    lines: VecDeque<String>,
    bytes: usize,
}

impl std::fmt::Debug for LogRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let lines = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .lines
            .len();
        f.debug_struct("LogRing").field("lines", &lines).finish()
    }
}

impl LogRing {
    fn push(&self, line: &str) {
        let mut line = redact(&strip_ansi(line));
        if line.len() > MAX_LINE_BYTES {
            let end = (0..=MAX_LINE_BYTES)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0);
            line.truncate(end);
            line.push('…');
        }
        let mut ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        ring.bytes += line.len();
        ring.lines.push_back(line);
        while ring.lines.len() > MAX_LINES || ring.bytes > MAX_BYTES {
            let Some(old) = ring.lines.pop_front() else {
                break;
            };
            ring.bytes -= old.len();
        }
    }

    /// The latest `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let ring = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let skip = ring.lines.len().saturating_sub(n);
        ring.lines.iter().skip(skip).cloned().collect()
    }
}

/// Collects one formatted event and adds its lines to the ring when dropped.
pub struct LineWriter<'a> {
    ring: &'a LogRing,
    buf: Vec<u8>,
}

impl std::fmt::Debug for LineWriter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineWriter").finish_non_exhaustive()
    }
}

impl io::Write for LineWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LineWriter<'_> {
    fn drop(&mut self) {
        for line in String::from_utf8_lossy(&self.buf).lines() {
            if !line.trim().is_empty() {
                self.ring.push(line);
            }
        }
    }
}

impl<'a> MakeWriter<'a> for LogRing {
    type Writer = LineWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        LineWriter {
            ring: self,
            buf: Vec::new(),
        }
    }
}

/// A `fmt` layer writing plain lines into `ring`, next to the one writing to stdout.
pub fn layer<S>(ring: LogRing) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(ring)
}

/// Removes ANSI escape sequences (`ESC [ ... final byte`), which field values may carry.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// Prefixes of keys and tokens the daemon handles: provider keys (`sk-` covers `sk-ant-` and
/// `sk-or-`), Gemini (`AIza`), Google access tokens (`ya29.`), GitHub, and JWTs (`eyJ`).
const KEY_PREFIXES: [&str; 9] = [
    "sk-",
    "sk_",
    "xi-",
    "AIza",
    "ya29.",
    "ghp_",
    "gho_",
    "github_pat_",
    "eyJ",
];
/// A `<name>=` or `<name>: ` whose name ends in one of these is followed by a secret.
const SECRET_NAMES: [&str; 4] = ["token", "key", "secret", "password"];

/// Replaces what looks like a credential with `[REDACTED]`. Logs never hold keys; this is
/// defence in depth before lines leave the daemon.
fn redact(line: &str) -> String {
    // ASCII lowercasing keeps byte offsets, and words below only end at ASCII bytes.
    let lower = line.to_ascii_lowercase();
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let (mut copied, mut i) = (0, 0);
    while i < bytes.len() {
        if !is_word_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word_byte(bytes[i]) {
            i += 1;
        }
        let word = &line[start..i];
        let before = &lower[..start];
        let secret = KEY_PREFIXES
            .iter()
            .any(|p| word.starts_with(p) && word.len() >= p.len() + 12)
            || (word.len() >= 8 && before.ends_with("bearer "))
            || (word.len() >= 16 && follows_secret_name(before));
        if secret {
            out.push_str(&line[copied..start]);
            out.push_str(REDACTED);
            copied = i;
        }
    }
    out.push_str(&line[copied..]);
    out
}

/// Token and base64 characters (`=` excluded: it separates a name from its value).
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_.+/~".contains(&b)
}

/// Whether `before` ends in `token=`, `api_key: "`, `"secret":"` and the like.
fn follows_secret_name(before: &str) -> bool {
    let quotes: &[char] = &['"', '\'', ' '];
    before
        .trim_end_matches(quotes)
        .strip_suffix(['=', ':'])
        .map(|name| name.trim_end_matches(quotes))
        .is_some_and(|name| SECRET_NAMES.iter().any(|n| name.ends_with(n)))
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    #[test]
    fn redacts_what_looks_like_a_credential() {
        let key = "sk-ant-api03-AbCdEf0123456789_xyz";
        for (line, want) in [
            (
                "Authorization: Bearer abcdef0123456789",
                "Authorization: Bearer [REDACTED]",
            ),
            (
                &format!("upstream said {key} is bad"),
                "upstream said [REDACTED] is bad",
            ),
            ("key=sk-or-v1-0123456789abcdef", "key=[REDACTED]"),
            (
                "xi-0123456789abcdef and ghp_0123456789abcdefXYZ",
                "[REDACTED] and [REDACTED]",
            ),
            (
                "AIzaSyA0123456789abcdefghij, ya29.a0AfB_0123456789xyz",
                "[REDACTED], [REDACTED]",
            ),
            (
                "jwt eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ4In0.c2lnbmF0dXJl ok",
                "jwt [REDACTED] ok",
            ),
            (
                "GET /v1/ws?access_token=test-operator-token-0123456789&x=1",
                "GET /v1/ws?access_token=[REDACTED]&x=1",
            ),
            (
                r#"Settings { api_key: "0123456789abcdef0123" }"#,
                r#"Settings { api_key: "[REDACTED]" }"#,
            ),
            (
                r#"{"secret":"QUJDREVGR0hJSktMTU5PUA=="}"#,
                r#"{"secret":"[REDACTED]=="}"#,
            ),
            // Ordinary words, ids and short values stay.
            (
                "task-runner disk-full sk-learn",
                "task-runner disk-full sk-learn",
            ),
            (
                "send as `Authorization: Bearer <token>`",
                "send as `Authorization: Bearer <token>`",
            ),
            (
                "key=archive-key failed: key: not found",
                "key=archive-key failed: key: not found",
            ),
            (
                "agent 0f6c2a7e-51a2-4f4b-9b1e-2d7c1f3b9a10 woke",
                "agent 0f6c2a7e-51a2-4f4b-9b1e-2d7c1f3b9a10 woke",
            ),
            ("größer key=ünicode", "größer key=ünicode"),
        ] {
            assert_eq!(redact(line), want, "{line}");
        }
    }

    #[test]
    fn strips_colour_codes() {
        assert_eq!(
            strip_ansi("\x1b[2m2026\x1b[0m \x1b[32m INFO\x1b[0m ok"),
            "2026  INFO ok"
        );
    }

    #[test]
    fn keeps_the_latest_lines_within_bounds() {
        let ring = LogRing::default();
        for i in 0..MAX_LINES + 5 {
            ring.push(&format!("line {i}"));
        }
        let all = ring.tail(usize::MAX);
        assert_eq!(all.len(), MAX_LINES);
        assert_eq!(all.last().unwrap(), &format!("line {}", MAX_LINES + 4));
        assert_eq!(
            ring.tail(2),
            [
                format!("line {}", MAX_LINES + 3),
                format!("line {}", MAX_LINES + 4)
            ]
        );

        let ring = LogRing::default();
        ring.push(&"é".repeat(MAX_LINE_BYTES));
        let long = &ring.tail(1)[0];
        assert!(long.len() <= MAX_LINE_BYTES + '…'.len_utf8() && long.ends_with('…'));
        for _ in 0..MAX_BYTES / MAX_LINE_BYTES + 10 {
            ring.push(&"x".repeat(MAX_LINE_BYTES));
        }
        let ring = ring.0.lock().unwrap();
        assert!(ring.bytes <= MAX_BYTES);
        assert_eq!(
            ring.bytes,
            ring.lines.iter().map(String::len).sum::<usize>()
        );
    }

    #[test]
    fn the_layer_writes_plain_redacted_lines() {
        let ring = LogRing::default();
        let subscriber = tracing_subscriber::registry().with(layer(ring.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(token = "abcdef0123456789abcdef", "two\nlines");
        });
        let lines = ring.tail(10);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].contains("WARN") && lines[0].ends_with("two"),
            "{lines:?}"
        );
        assert_eq!(lines[1], "lines token=\"[REDACTED]\"");
    }
}
