The daemon counts what every model call uses and shows it in the **Usage**
view (chart icon at the top of the sidebar), with cost estimates.

## What is recorded

The [[model proxy|Architecture]] writes one row per model call:

- time, agent, provider, model
- input tokens (without cache reads and writes), output tokens
- cache read and cache write tokens, where the provider reports them
- latency
- status: `ok`, `refused` (the proxy turned the call away) or `error`

Only metadata and counts are stored, never prompts or completions. Model
lists and token-count calls are not logged. Rows outlive deleted agents.

## Where the counts come from

- Counts come from the usage fields in the provider's response. They are
  never estimated or computed from text.
- When a response carries no usage, the row is **not tracked** instead of a
  number. Today that is Bedrock's binary streaming format and
  OpenAI-compatible servers that ignore `stream_options.include_usage`.
- Every driver reaches its model through the proxy, including Codex with a
  ChatGPT sign-in and Gemini CLI.

| API | Counted from |
| --- | --- |
| Anthropic, Vertex, Bedrock (JSON) | `usage` (stream: `message_start` and `message_delta`) |
| OpenAI chat and compatible servers | `usage` in the last stream chunk |
| Codex (Responses API) | `response.completed` |
| Gemini | `usageMetadata`; output includes thinking tokens |

## Prices

- Prices live in `config/pricing.toml`: USD per 1M input, output, cache-read
  and cache-write tokens. Each entry has a `checked` date and the source URL
  it was taken from.
- You can set your own price per model from the Usage view; your prices win
  over the file.
- Local models (Ollama) cost 0 unless you set a price.
- A model with no price shows its tokens, and its cost as **unknown**.
- Not modelled: long-prompt price tiers, batch discounts, 1-hour cache writes.
  A ChatGPT sign-in is not billed per token, so its estimate overstates it.
- **All costs are estimates.** Your provider's bill is what counts.

## The Usage view

- Ranges: today, 7 days, 30 days or a custom range (local days).
- The range's total cost, with the number of calls.
- Each provider with its colour, cost, share of the total and tokens.
- Cost per day, stacked by provider (tokens when no cost is known), in the
  daemon's time zone.
- Token totals: without cache reads, cache reads, uncached input, cache
  writes and output.
- A sortable breakdown with Model, Agent and Day tabs (Day rows are UTC
  dates); set your own price with the pencil on the Model tab.
- CSV export of the open tab.
- With a Claude Code provider, a note that its costs are estimates at API
  prices, which your Claude subscription covers.

## Plan limits

With a Claude Code provider, or a Codex provider signed in with ChatGPT, the
Usage view shows a card per plan under its header, with a bar each for the
**5-hour window** and **Weekly**: the percentage used and when it resets. A
plan whose limits could not be read says why, with **Try again**.

- Claude Code: the daemon runs the host's own `claude` with its `/usage`
  command (no model call, no cost) in an empty folder, and keeps only the
  "Current session" and "Current week (all models)" lines. Every Claude Code
  provider shares the one login.
- Codex: the host's `codex app-server` reads the signed-in account's rate
  limits (`account/rateLimits/read`). Providers with an API key have no plan
  and are left out.
- Readings are cached for a minute. Account details are never returned.

## Context meter

The ring by a chat's message box shows how full the agent's context is: the
input, cache read and cache write tokens of its newest proxied call, against
the model's window (Claude 4.6 and later, Fable and Gemini 1M, older Claude
200K, GPT-5 400K; other models say "unknown"). Claude Code agents run on your
own login outside the proxy, so their ring comes from the token counts
`claude` reports for its newest call (their cost stays untracked). New chat in its
popover makes the agent's next turn start a fresh session; the chat history
stays. **Compact now** (Claude Code agents) has the daemon run `claude`'s own
`/compact` on the agent's session, so its model keeps a summary instead of
the whole history; it never runs from message text.

The popover also shows what fills the context, as a stacked bar and a row per
part (system prompt, system tools, MCP tools, messages, free space), with the
largest MCP tools under **MCP tools**. The total stays the exact count; the
parts are estimates:

- **Claude Code agents**: after each turn the daemon runs `claude`'s own
  `/context` on the agent's session with the turn's MCP config, system prompt,
  plugins and model. It is a local command (no model call), runs no hooks and
  keeps the report out of the session (`--no-session-persistence`). The parts
  are Claude Code's own, rounded (`2.5k`), and include its memory files,
  skills, custom agents, deferred tools and autocompact buffer when there are
  any. They show a moment after the turn ends. **Measure now** in the popover
  fills in a turn without a breakdown (one that ran on an older daemon, or
  whose measurement failed): the same `/context`, run on request without the
  agent's MCP tools, which only a turn can reach (its token exists only then).
  The popover says what was left out; the next turn counts everything again.
- **Agents behind the model proxy** (Claude, OpenAI-compatible, Codex, Gemini
  CLI): the proxy measures the size of the newest request's system prompt, tool
  definitions and messages (images at a flat size, never their text) and splits
  the API's reported input tokens by them. Tools named `mcp__ferrobot__…` are
  MCP tools, the rest system tools. The sizes are kept in memory only, so after
  a daemon restart the breakdown comes back with the next call. Calls whose API
  reports no tokens (Bedrock streaming) have none.

## API

All routes need the operator token; the shapes are in `protocol/openapi.json`.

- `GET /v1/usage?from&to&agent&provider&model&group_by` returns totals and
  groups (`group_by` is `day`, `agent`, `provider` or `model`). Each has the
  call count, the four token counts, `cost_usd` (`null` when nothing could be
  priced), `unknown_cost_calls` and `not_tracked_calls`.
- `GET /v1/usage/prices` returns the prices in effect, with source and date.
- `PUT /v1/usage/prices` replaces your price overrides.
- `GET /v1/limits` returns, per Claude Code or ChatGPT-signed-in Codex
  provider, `five_hour` and `weekly` (`used_percent`, `resets`: Claude Code's
  own words or an RFC 3339 time for Codex), or an `error`.
- `GET /v1/agents/{id}/context` returns `tracked`, `tokens`, `window`, `model`
  and `at` for the context meter, and `parts` (`kind`, `label`, `tokens`,
  `deferred`), `tools` (MCP tools by size) and `source` (`claude_code` or
  `request_size`; `null` when not measured) and `left_out` for its breakdown.
- `POST /v1/agents/{id}/context/measure` (Claude Code agents) measures now,
  without the MCP tools, and returns the same; a full breakdown from the
  newest turn is kept and returned as it is.
