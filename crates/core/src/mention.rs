//! `@name` mentions, stored by agent id so they survive renames.
//!
//! When a text is persisted, `@name` (case-insensitive) is replaced with the
//! token `<@agent-id>` if an agent of that name exists; unknown names stay
//! plain text. Clients render tokens with the agent's current name; texts
//! handed to a model get them back as `@current-name` ([`render`]).

use crate::AgentId;

const TOKEN_OPEN: &str = "<@";
/// Length of a hyphenated UUID.
const ID_LEN: usize = 36;

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// Replaces `@name` with `<@id>` where `lookup` (given the lower-cased name)
/// knows the agent. A mention must not follow a word character (so e-mail
/// addresses are left alone).
pub fn normalize(text: &str, lookup: impl Fn(&str) -> Option<AgentId>) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        let starts_mention = bytes[i] == b'@'
            && (i == 0
                || !(bytes[i - 1].is_ascii_alphanumeric() || b"_.<".contains(&bytes[i - 1])))
            && bytes.get(i + 1).is_some_and(u8::is_ascii_alphabetic);
        if !starts_mention {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && is_name_byte(bytes[end]) {
            end += 1;
        }
        while bytes[end - 1] == b'-' {
            end -= 1;
        }
        if let Some(id) = lookup(&text[start..end].to_ascii_lowercase()) {
            out.push_str(&text[copied..i]);
            out.push_str(TOKEN_OPEN);
            out.push_str(&id.to_string());
            out.push('>');
            copied = end;
        }
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Splits a stored text into plain parts and mention tokens.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(TOKEN_OPEN) {
        let after = &rest[pos + TOKEN_OPEN.len()..];
        let id = after
            .get(..ID_LEN)
            .filter(|_| after.as_bytes().get(ID_LEN) == Some(&b'>'))
            .and_then(|s| s.parse::<AgentId>().ok());
        if let Some(id) = id {
            if pos > 0 {
                out.push(Segment::Text(&rest[..pos]));
            }
            out.push(Segment::Mention(id));
            rest = &after[ID_LEN + 1..];
        } else {
            out.push(Segment::Text(&rest[..pos + TOKEN_OPEN.len()]));
            rest = after;
        }
    }
    if !rest.is_empty() {
        out.push(Segment::Text(rest));
    }
    out
}

/// A piece of a stored text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    Mention(AgentId),
}

/// Turns tokens back into `@name` with each agent's current name
/// (`@unknown-agent` for deleted ones), e.g. for text sent to a model.
pub fn render(text: &str, name_of: impl Fn(AgentId) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    for seg in segments(text) {
        match seg {
            Segment::Text(t) => out.push_str(t),
            Segment::Mention(id) => {
                out.push('@');
                out.push_str(&name_of(id).unwrap_or_else(|| "unknown-agent".into()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_known_names_only() {
        let bob = AgentId::new();
        let lookup = |n: &str| (n == "bob").then_some(bob);
        let text = "@Bob, ask @nobody; mail bob@bob.dev or (@bob-). @bob";
        let got = normalize(text, lookup);
        assert_eq!(
            got,
            format!("<@{bob}>, ask @nobody; mail bob@bob.dev or (<@{bob}>-). <@{bob}>")
        );
        // Already normalized text is left alone.
        assert_eq!(normalize(&got, lookup), got);
        assert_eq!(normalize("ä @ @1x @", lookup), "ä @ @1x @");
    }

    #[test]
    fn renders_current_names() {
        let bob = AgentId::new();
        let gone = AgentId::new();
        let text = format!("hi <@{bob}> and <@{gone}>, <@not-an-id> <@");
        let name_of = |id: AgentId| (id == bob).then(|| "robert".to_owned());
        assert_eq!(
            render(&text, name_of),
            "hi @robert and @unknown-agent, <@not-an-id> <@"
        );
        assert_eq!(segments(&format!("<@{bob}>")), [Segment::Mention(bob)]);
    }
}
