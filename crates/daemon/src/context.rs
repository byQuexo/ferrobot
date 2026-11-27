//! What fills an agent's context: the system prompt, tools and messages of its newest model call.
//! For Claude Code agents from `claude`'s own `/context` report after each turn; for agents
//! behind the model proxy estimated from the size of each part of the request it forwarded,
//! scaled to the token count the API reported. Only sizes and tool names are kept, never text.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use ferrobot_core::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

use crate::usage::Family;

/// What a part of the context holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextPartKind {
    /// The harness's own instructions plus ferrobot's (the agent's instructions, its team and
    /// the knowledge base).
    SystemPrompt,
    /// The harness's built-in tools (shell, files, web).
    SystemTools,
    /// ferrobot's tools and connector tools, served over MCP.
    McpTools,
    /// Subagent descriptions (Claude Code).
    CustomAgents,
    /// Memory files such as `CLAUDE.md` (Claude Code).
    MemoryFiles,
    /// Skill descriptions (Claude Code).
    Skills,
    /// The conversation: messages, tool calls and their results.
    Messages,
    /// Room Claude Code keeps free so it can compact before the window is full.
    AutocompactBuffer,
    /// What is left of the window.
    FreeSpace,
    /// A part this daemon does not know by name; see `label`.
    Other,
}

/// One part of the context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContextPart {
    pub kind: ContextPartKind,
    /// The part's name as measured, e.g. `MCP tools (deferred)`.
    pub label: String,
    pub tokens: i64,
    /// Tools the model sees only by name until it looks one up: listed, but not in the window.
    pub deferred: bool,
}

/// One MCP tool's definition and its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ContextTool {
    /// Tool name without the `mcp__ferrobot__` prefix.
    pub name: String,
    pub tokens: i64,
}

/// How the parts were measured. Both are estimates; the total beside them is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextSource {
    /// Claude Code's own `/context` after the agent's newest turn (numbers rounded, e.g. 2.5k).
    ClaudeCode,
    /// The size of each part of the newest request through the model proxy, scaled to the input
    /// tokens the API reported for it.
    RequestSize,
}

/// What a measurement could not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextLeftOut {
    /// The agent's MCP tools: measured on request, outside a turn, `claude` cannot reach them
    /// (the agent's token exists only during a turn).
    McpTools,
    /// ferrobot's instructions in the system prompt: none were kept from the agent's turns yet.
    Instructions,
}

/// A context split into parts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakdown {
    pub source: ContextSource,
    pub parts: Vec<ContextPart>,
    /// MCP tools, largest first.
    pub tools: Vec<ContextTool>,
    /// What this measurement could not count; empty when it counted everything.
    #[serde(default)]
    pub left_out: Vec<ContextLeftOut>,
}

// --- Claude Code: `claude -p --output-format=json /context` ---

/// Most rows kept from a report; the rest are dropped.
const MAX_ROWS: usize = 100;
/// Longest label or tool name kept.
const MAX_NAME: usize = 120;

/// Text from the host's `claude`, made plain: no control characters, at most [`MAX_NAME`] bytes.
fn plain(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        out.push(c);
    }
    out
}

/// `2.5k`, `964.5k`, `1m`, `1,234` or `10` as a token count.
fn tokens_of(s: &str) -> Option<i64> {
    let s = s.trim().replace(',', "");
    let (number, scale) = match s.chars().last()? {
        'k' | 'K' => (&s[..s.len() - 1], 1_000.0),
        'm' | 'M' => (&s[..s.len() - 1], 1_000_000.0),
        _ => (s.as_str(), 1.0),
    };
    let n: f64 = number.trim().parse().ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to the u32 range first"
    )]
    (n.is_finite() && n >= 0.0).then(|| (n * scale).round().min(f64::from(u32::MAX)) as i64)
}

fn kind_of(label: &str) -> ContextPartKind {
    match label.to_ascii_lowercase().as_str() {
        "system prompt" => ContextPartKind::SystemPrompt,
        "system tools" => ContextPartKind::SystemTools,
        "mcp tools" => ContextPartKind::McpTools,
        "custom agents" => ContextPartKind::CustomAgents,
        "memory files" => ContextPartKind::MemoryFiles,
        "skills" => ContextPartKind::Skills,
        "messages" => ContextPartKind::Messages,
        "autocompact buffer" => ContextPartKind::AutocompactBuffer,
        "free space" => ContextPartKind::FreeSpace,
        _ => ContextPartKind::Other,
    }
}

/// A tool name without ferrobot's server prefix.
fn tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__ferrobot__")
        .or_else(|| name.strip_prefix("ferrobot__"))
        .unwrap_or(name)
}

/// The cells of a Markdown table row; `None` for other lines, the header and its rule.
fn cells(line: &str) -> Option<Vec<&str>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let cells: Vec<&str> = inner.split('|').map(str::trim).collect();
    let rule = cells
        .iter()
        .all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':')));
    (!rule).then_some(cells)
}

/// Reads `claude`'s `/context` report: its category table and its MCP tools table.
pub fn parse_report(report: &str) -> Option<Breakdown> {
    #[derive(PartialEq)]
    enum Section {
        None,
        Categories,
        Tools,
    }
    let mut section = Section::None;
    let mut parts = Vec::new();
    let mut tools = Vec::new();
    for line in report.lines() {
        if let Some(heading) = line.trim().strip_prefix("###") {
            let heading = heading.trim().to_ascii_lowercase();
            section = if heading.contains("by category") {
                Section::Categories
            } else if heading == "mcp tools" {
                Section::Tools
            } else {
                Section::None
            };
            continue;
        }
        let Some(row) = cells(line) else {
            continue;
        };
        match section {
            Section::Categories if parts.len() < MAX_ROWS => {
                let (Some(name), Some(tokens)) = (row.first(), row.get(1)) else {
                    continue;
                };
                let Some(tokens) = tokens_of(tokens) else {
                    // The header row: `| Category | Tokens | Percentage |`.
                    continue;
                };
                let (base, deferred) = match name.strip_suffix("(deferred)") {
                    Some(base) => (base.trim(), true),
                    None => (*name, false),
                };
                parts.push(ContextPart {
                    kind: kind_of(base),
                    label: plain(name),
                    tokens,
                    deferred,
                });
            }
            Section::Tools if tools.len() < MAX_ROWS => {
                let (Some(name), Some(tokens)) =
                    (row.first(), row.last().and_then(|t| tokens_of(t)))
                else {
                    continue;
                };
                if row.len() >= 2 {
                    tools.push(ContextTool {
                        name: plain(tool_name(name)),
                        tokens,
                    });
                }
            }
            _ => {}
        }
    }
    tools.sort_by_key(|t| std::cmp::Reverse(t.tokens));
    (!parts.is_empty()).then_some(Breakdown {
        source: ContextSource::ClaudeCode,
        parts,
        tools,
        left_out: Vec::new(),
    })
}

// --- The model proxy: the request's parts by size ---

/// What an image or document counts as: about 1,500 tokens at four bytes a token.
const MEDIA: u64 = 6_000;
/// Inline data longer than this is media, not text.
const MEDIA_MIN: usize = 1_024;

/// About how many bytes `v` adds to a request, with inline media at a flat size and thinking
/// signatures left out (neither is read as text).
fn size(v: &Value) -> u64 {
    let len = |n: usize| u64::try_from(n).unwrap_or(u64::MAX);
    match v {
        Value::String(s) if s.len() > MEDIA_MIN && s.starts_with("data:") => MEDIA,
        Value::String(s) => len(s.len()),
        Value::Array(items) => items.iter().map(size).fold(2, u64::saturating_add),
        Value::Object(map) => map.iter().fold(2, |sum, (k, v)| {
            let field = match (k.as_str(), v) {
                ("signature" | "encrypted_content", _) => 0,
                ("data", Value::String(s)) if s.len() > MEDIA_MIN => MEDIA,
                _ => len(k.len()).saturating_add(size(v)),
            };
            sum.saturating_add(field)
        }),
        other => len(other.to_string().len()),
    }
}

/// The sizes of one request's parts.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Sizes {
    system: u64,
    messages: u64,
    /// The harness's own tools, summed.
    system_tools: u64,
    /// ferrobot's tools by name.
    mcp_tools: Vec<(String, u64)>,
}

impl Sizes {
    fn tool(&mut self, name: &str, v: &Value) {
        if name.starts_with("mcp__") || name.starts_with("ferrobot__") {
            self.mcp_tools.push((tool_name(name).to_owned(), size(v)));
        } else {
            self.system_tools = self.system_tools.saturating_add(size(v));
        }
    }

    fn total(&self) -> u64 {
        self.mcp_tools.iter().map(|(_, s)| *s).fold(
            self.system + self.messages + self.system_tools,
            u64::saturating_add,
        )
    }
}

/// System, tool and message items of a list where some roles are instructions.
fn split_roles(items: &Value, sizes: &mut Sizes) {
    for item in items.as_array().map_or(&[][..], Vec::as_slice) {
        if matches!(item["role"].as_str(), Some("system" | "developer")) {
            sizes.system += size(item);
        } else {
            sizes.messages += size(item);
        }
    }
}

/// Measures a model request's parts; `None` when the body is not a request of `family`.
pub(crate) fn measure(family: Family, body: &[u8]) -> Option<Sizes> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let mut sizes = Sizes::default();
    let tools = |key: &str| v[key].as_array().map_or(&[][..], Vec::as_slice);
    match family {
        Family::Anthropic => {
            v.get("messages")?;
            sizes.system = v.get("system").map_or(0, size);
            sizes.messages = size(&v["messages"]);
            for t in tools("tools") {
                sizes.tool(t["name"].as_str().unwrap_or_default(), t);
            }
        }
        Family::ChatCompletions => {
            split_roles(v.get("messages")?, &mut sizes);
            for t in tools("tools") {
                let name = t["function"]["name"].as_str().or(t["name"].as_str());
                sizes.tool(name.unwrap_or_default(), t);
            }
        }
        Family::Responses => {
            let input = v.get("input")?;
            sizes.system = v.get("instructions").map_or(0, size);
            if input.is_string() {
                sizes.messages = size(input);
            } else {
                split_roles(input, &mut sizes);
            }
            for t in tools("tools") {
                let name = t["name"].as_str().or(t["type"].as_str());
                sizes.tool(name.unwrap_or_default(), t);
            }
        }
        Family::Gemini => {
            sizes.messages = size(v.get("contents")?);
            sizes.system = v.get("systemInstruction").map_or(0, size);
            for t in tools("tools") {
                match t["functionDeclarations"].as_array() {
                    Some(declared) => {
                        for f in declared {
                            sizes.tool(f["name"].as_str().unwrap_or_default(), f);
                        }
                    }
                    None => sizes.tool("", t),
                }
            }
        }
    }
    Some(sizes)
}

/// `part` of `total` tokens, as `size` is of `of`.
fn share(total: i64, size: u64, of: u64) -> i64 {
    if of == 0 {
        return 0;
    }
    let exact = i128::from(total) * i128::from(size) / i128::from(of);
    i64::try_from(exact).unwrap_or(i64::MAX)
}

/// Splits `tokens` (the call's whole input) by the request's sizes; messages take the rounding.
pub(crate) fn estimate(sizes: &Sizes, tokens: i64) -> Breakdown {
    let of = sizes.total();
    let mut tools: Vec<ContextTool> = sizes
        .mcp_tools
        .iter()
        .map(|(name, s)| ContextTool {
            name: name.clone(),
            tokens: share(tokens, *s, of),
        })
        .collect();
    tools.sort_by_key(|t| std::cmp::Reverse(t.tokens));
    let part = |kind, label: &str, tokens| ContextPart {
        kind,
        label: label.into(),
        tokens,
        deferred: false,
    };
    let system = share(tokens, sizes.system, of);
    let system_tools = share(tokens, sizes.system_tools, of);
    let mcp: i64 = tools.iter().map(|t| t.tokens).sum();
    let mut parts = vec![part(ContextPartKind::SystemPrompt, "System prompt", system)];
    if sizes.system_tools > 0 {
        parts.push(part(
            ContextPartKind::SystemTools,
            "System tools",
            system_tools,
        ));
    }
    if !sizes.mcp_tools.is_empty() {
        parts.push(part(ContextPartKind::McpTools, "MCP tools", mcp));
    }
    let messages = (tokens - system - system_tools - mcp).max(0);
    parts.push(part(ContextPartKind::Messages, "Messages", messages));
    Breakdown {
        source: ContextSource::RequestSize,
        parts,
        tools,
        left_out: Vec::new(),
    }
}

/// The newest measured proxy call per agent, kept in memory only (gone after a restart).
struct Measured {
    model: String,
    tokens: i64,
    sizes: Sizes,
}

static LATEST: LazyLock<Mutex<HashMap<AgentId, Measured>>> = LazyLock::new(Mutex::default);

/// Keeps a finished call's sizes; `tokens` is its input plus cache read and write.
pub(crate) fn remember(agent: AgentId, model: &str, tokens: i64, sizes: Sizes) {
    crate::lock(&LATEST).insert(
        agent,
        Measured {
            model: model.to_owned(),
            tokens,
            sizes,
        },
    );
}

/// The breakdown of the agent's newest measured call, if it is the call with this model and
/// token count (the one the context meter shows).
pub(crate) fn of_call(agent: AgentId, model: &str, tokens: i64) -> Option<Breakdown> {
    let latest = crate::lock(&LATEST);
    let m = latest
        .get(&agent)
        .filter(|m| m.model == model && m.tokens == tokens)?;
    Some(estimate(&m.sizes, tokens))
}

/// Drops a deleted agent's entry.
pub(crate) fn forget(agent: AgentId) {
    crate::lock(&LATEST).remove(&agent);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const REPORT: &str = "## Context Usage\n\n**Model:** claude-opus-5-5  \n**Tokens:** 6.6k / 1m (1%)\n\n\
### Estimated usage by category\n\n| Category | Tokens | Percentage |\n|----------|--------|------------|\n\
| System prompt | 4.3k | 0.4% |\n| System tools | 2k | 0.2% |\n| MCP tools | 218 | 0.0% |\n\
| MCP tools (deferred) | 1.1k | — |\n| Messages | 10 | 0.0% |\n| Free space | 960.5k | 96.0% |\n\
| Autocompact buffer | 33k | 3.3% |\n\n### MCP Tools\n\n| Tool | Server | Tokens |\n|------|--------|--------|\n\
| mcp__ferrobot__send_message | ferrobot | 33 |\n| mcp__ferrobot__bash | ferrobot | 185 |\n";

    #[test]
    fn claude_codes_report_becomes_parts_and_tools() {
        let b = parse_report(REPORT).unwrap();
        assert_eq!(b.source, ContextSource::ClaudeCode);
        let got: Vec<_> = b
            .parts
            .iter()
            .map(|p| (p.kind, p.tokens, p.deferred))
            .collect();
        assert_eq!(
            got,
            [
                (ContextPartKind::SystemPrompt, 4_300, false),
                (ContextPartKind::SystemTools, 2_000, false),
                (ContextPartKind::McpTools, 218, false),
                (ContextPartKind::McpTools, 1_100, true),
                (ContextPartKind::Messages, 10, false),
                (ContextPartKind::FreeSpace, 960_500, false),
                (ContextPartKind::AutocompactBuffer, 33_000, false),
            ]
        );
        assert_eq!(b.parts[3].label, "MCP tools (deferred)");
        assert_eq!(
            b.tools,
            [
                ContextTool {
                    name: "bash".into(),
                    tokens: 185
                },
                ContextTool {
                    name: "send_message".into(),
                    tokens: 33
                },
            ],
            "largest first, without the server prefix"
        );
        assert_eq!(parse_report("Unknown command: /context"), None);
    }

    #[test]
    fn report_numbers_and_names_are_read_carefully() {
        assert_eq!(tokens_of("2.5k"), Some(2_500));
        assert_eq!(tokens_of("1m"), Some(1_000_000));
        assert_eq!(tokens_of("1,234"), Some(1_234));
        assert_eq!(tokens_of("1.5M"), Some(1_500_000));
        assert_eq!(tokens_of("Tokens"), None);
        assert_eq!(tokens_of("-3"), None);
        assert_eq!(tokens_of("1e30"), Some(i64::from(u32::MAX)));
        let report = format!(
            "### Estimated usage by category\n| Warp\u{1b}[31m drive | 5 | 1% |\n| {} | 1 | 0% |\n",
            "x".repeat(500)
        );
        let b = parse_report(&report).unwrap();
        assert_eq!(b.parts[0].kind, ContextPartKind::Other);
        assert_eq!(b.parts[0].label, "Warp[31m drive", "no control characters");
        assert_eq!(b.parts[1].label.len(), MAX_NAME);
    }

    #[test]
    fn anthropic_requests_split_by_size_and_add_up() {
        let body = json!({
            "model": "claude-opus-5-5",
            "system": [{"type": "text", "text": "s".repeat(1000)}],
            "tools": [
                {"name": "Bash", "description": "b".repeat(500), "input_schema": {}},
                {"name": "mcp__ferrobot__send_message", "description": "m".repeat(300), "input_schema": {}},
            ],
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "u".repeat(2000)},
                    {"type": "image", "source": {"type": "base64", "data": "A".repeat(500_000)}},
                ]},
                {"role": "assistant", "content": [{"type": "thinking", "thinking": "t", "signature": "S".repeat(9000)}]},
            ],
        });
        let sizes = measure(Family::Anthropic, body.to_string().as_bytes()).unwrap();
        assert_eq!(sizes.mcp_tools.len(), 1);
        assert!(
            sizes.messages < 10_000,
            "an image is not its bytes: {sizes:?}"
        );
        let b = estimate(&sizes, 10_000);
        assert_eq!(b.source, ContextSource::RequestSize);
        assert_eq!(b.parts.iter().map(|p| p.tokens).sum::<i64>(), 10_000);
        let kinds: Vec<_> = b.parts.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            [
                ContextPartKind::SystemPrompt,
                ContextPartKind::SystemTools,
                ContextPartKind::McpTools,
                ContextPartKind::Messages
            ]
        );
        assert_eq!(b.tools[0].name, "send_message");
        assert_eq!(b.tools[0].tokens, b.parts[2].tokens);
        assert!(b.parts[3].tokens > b.parts[0].tokens, "{b:?}");
        assert_eq!(measure(Family::Anthropic, b"{\"model\": \"x\"}"), None);
        assert_eq!(measure(Family::Anthropic, b"not json"), None);
    }

    #[test]
    fn other_apis_find_their_system_tools_and_messages() {
        let chat = json!({
            "messages": [{"role": "system", "content": "s".repeat(400)},
                         {"role": "user", "content": "u".repeat(400)}],
            "tools": [{"type": "function", "function": {"name": "mcp__ferrobot__remember"}},
                      {"type": "function", "function": {"name": "shell"}}],
        });
        let s = measure(Family::ChatCompletions, chat.to_string().as_bytes()).unwrap();
        assert!(s.system > 400 && s.messages > 400 && s.system_tools > 0);
        assert_eq!(s.mcp_tools[0].0, "remember");

        let responses = json!({
            "instructions": "i".repeat(300),
            "input": [{"role": "developer", "content": "d"}, {"type": "message", "role": "user", "content": "u"}],
            "tools": [{"type": "function", "name": "ferrobot__ask_user"}, {"type": "web_search"}],
        });
        let s = measure(Family::Responses, responses.to_string().as_bytes()).unwrap();
        assert!(s.system > 300 && s.messages > 0 && s.system_tools > 0);
        assert_eq!(s.mcp_tools[0].0, "ask_user");

        let gemini = json!({
            "systemInstruction": {"parts": [{"text": "s"}]},
            "contents": [{"role": "user", "parts": [{"inlineData": {"data": "A".repeat(5000)}}]}],
            "tools": [{"functionDeclarations": [{"name": "mcp__ferrobot__bash"}, {"name": "read_file"}]}],
        });
        let s = measure(Family::Gemini, gemini.to_string().as_bytes()).unwrap();
        assert_eq!(s.mcp_tools.len(), 1);
        assert!(s.system_tools > 0 && s.messages >= MEDIA);
    }

    #[test]
    fn only_the_call_the_meter_shows_gets_a_breakdown() {
        let agent = AgentId::new();
        let sizes = Sizes {
            system: 10,
            messages: 30,
            ..Sizes::default()
        };
        assert_eq!(of_call(agent, "m", 100), None);
        remember(agent, "m", 100, sizes);
        let b = of_call(agent, "m", 100).unwrap();
        assert_eq!((b.parts[0].tokens, b.parts[1].tokens), (25, 75));
        assert_eq!(of_call(agent, "m", 101), None, "another call");
        assert_eq!(of_call(agent, "small", 100), None, "a side call's model");
        forget(agent);
        assert_eq!(of_call(agent, "m", 100), None);
    }
}
