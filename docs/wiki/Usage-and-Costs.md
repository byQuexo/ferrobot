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

## Plan limits

With a Claude Code provider, or a Codex provider signed in with ChatGPT, the
Usage header shows two rings per provider: how much of the 5-hour window and
of the week is used. Hover a ring for its reset time.

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
own login outside the proxy, so they show "not tracked". New chat in its
popover makes the agent's next turn start a fresh session; the chat history
stays. Compacting a conversation on demand is not offered.

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
  and `at` for the context meter.
