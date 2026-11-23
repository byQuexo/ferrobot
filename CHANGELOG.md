# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Agent tags, a model switch in the chat header and a context meter. Up to five
  tags per agent show next to its name (`tags` on agents, migration v18). The
  chat header's model chip switches the model (the agent's computer restarts).
  A ring by the message box shows how full the agent's context is
  (`GET /v1/agents/{id}/context`, from its newest proxied call; Claude Code is
  not tracked) and offers New chat (`POST /v1/agents/{id}/new-session`): the
  agent's next turn starts a fresh model session (`new_session` on the runner's
  `turn` frame; older runners ignore it).

- Risky steps ask first: a rule-based classifier checks every shell command
  (all drivers) and connector tool call. Risky ones (deleting a folder,
  `git push`, sending data out, connector tools that send or change things)
  wait for an Approve/Decline card unless Auto mode is on; dangerous ones
  (deleting `/` or home, formatting a disk, piping a download into a shell)
  are always refused.
- Settings → Appearance: theme, accent color (8 swatches, readable in light
  and dark mode), your bubbles (solid, soft or plain), chat background
  (default, tinted or plain) and text size, saved on this computer.
- Keyboard shortcuts (⌘ on macOS, Ctrl elsewhere): ⌘K goes to an agent or a
  settings page, ⌘N, ⌘,, ⌘⇧U, ⌘I, ⌘B, ⌘1–9, Esc and ⌘/ for the list;
  Settings → Keyboard lists them all.
- Plan limits: with a Claude Code provider or a Codex provider signed in with
  ChatGPT, the Usage header shows rings for the 5-hour and weekly windows,
  with the reset time on hover (`GET /v1/limits`). Read from the host's own
  `claude` (`/usage`, no model call) and `codex app-server`; never the
  account.
- Richer chat: ```mermaid blocks draw as diagrams and ```html blocks as a
  sandboxed live preview with their styles and scripts, served by the daemon
  like shared pages (`POST /v1/previews`; Preview/Code, Copy, Expand); shared
  .html and .pdf files preview under their card; cleaner tables; a "New" line
  where unread turns start; mail with several agents folds into one
  "N messages with K agents" line.
- Group chats: a named room of two or more agents (`/v1/groups`). Write once
  and the members answer one at a time by the call-room rules; calling the
  group makes it the call's room. Rename, change members or delete it from
  the group's header. New migration: `rooms.name`.
- Files, reactions and subagents in the chat: attach files with the
  paperclip, drag and drop or paste (`POST /v1/agents/{id}/uploads` puts them
  in the agent's `uploads/` folder, at most 25 MB each); agents react to your
  messages with one emoji (`react` tool, `reaction` event); OpenAI-compatible
  agents get a `subagent` tool that runs tasks in parallel with the same tools.

### Changed

- Calmer, clearer desktop UI. Avatars show trouble and an open question card;
  the agent list shows what an agent is doing or its last reply with the time
  (`waiting` and `last_reply` on agents); finished tool runs fold into one
  "N steps · 40 s" line that opens Activities, with a short live trail while
  the agent works; a failed turn with no undelivered message leaves one quiet
  line; Activities lead with plain words, grouped by turn; the Computer tab
  starts with a plain summary; notices get icons; new items rise in; the middle
  view is a raised panel with fewer lines and stronger dark-mode layers; Edit
  moves out of the chat header (it stays in the Profile tab).
- The README demo records in dark mode.
- A clearer Usage page: the range's total cost up front, providers with their
  share, daily cost stacked by provider, a token totals row, and a breakdown
  with Model, Agent and Day tabs.

### Fixed

- Calls whose agents were all deleted no longer show as empty "Call" rows.

## [0.1.3] - 2026-11-23

### Added

- Claude Code (your own login), opt-in: a provider kind that runs the daemon
  computer's own, unmodified `claude` CLI, signed in with your own Claude
  subscription through Anthropic's flow. ferrobot never reads, stores or
  relays that login. `claude` runs on the host with its built-in tools off and
  none of your settings; the agent's computer is reached through ferrobot's
  `bash`, `read_file` and `write_file` MCP tools. Settings shows whether
  `claude` is installed and logged in (`GET /v1/claude-code`); Usage marks
  these agents as not tracked. All agents share your seat's limits.

## [0.1.2] - 2026-11-23

### Fixed

- First run with a Codex provider shows the ChatGPT sign-in once, in Ferro's
  line, instead of twice; the not-installed message is shorter and no longer
  sends you to Vault next to the key field.

## [0.1.1] - 2026-11-23

### Added

- Every model provider kind links to where its key is created or how it is
  set up: under the kind when adding a provider, and next to a missing key in
  Vault.

### Fixed

- First run with a Codex provider: Ferro no longer wakes on Codex before it
  can think. Without a key it asks for an OpenAI API key or a ChatGPT sign-in,
  and links the Codex install when Codex is missing on the computer.
- The daemon finds Codex where its installers and npm put it, even when the
  app was started from the Dock or the Start menu with a short PATH; on
  Windows it also runs npm's `codex.cmd`.
- An agent held for a ChatGPT sign-in links **Sign in with ChatGPT** instead
  of Vault.
- Dictation says right away when the voice provider has no key, instead of
  after you stop talking.
- A message that was not delivered shows why under it: the model provider's
  answer in plain words, e.g. "The model provider answered 429 (rate limit or
  quota reached): …". Google's errors are read too (they came through as raw
  JSON), and the daemon log notes each error status for Diagnostics.

## [0.1.0] - 2026-11-22

First public release.

### Added

- First run (desktop): with no agents, Ferro walks you through Docker and a
  model provider with its key, then wakes up as your main agent (`ferro`,
  pinned): it takes tasks and builds a team of agents when a job needs one.
  **Skip** sets up alone.
- Guides under Settings: the wiki's user pages (`docs/wiki`), bundled into the
  app. New pages on connectors and OAuth (with a Gmail example), CLIs, tools
  and permissions, routines and memory, voice and calls, and troubleshooting.
- A model that can't use tools (OpenRouter's "No endpoints found that support
  tool use", Ollama's "does not support tools") no longer fails the agent's
  messages: they wait, the agent shows **Model can't use tools**, and one card
  offers to run it without tools (`tools_unsupported` on the agent view).
- The agent sidebar collapses into a narrow rail of avatars (pinned agents
  first, starred), with usage, settings and new agent at the bottom and a call
  indicator while a call runs.
- Diagnostics under Settings: versions, Docker status, the runner image,
  counts, the data folder and the daemon's latest log lines, with **Copy
  diagnostics** for bug reports (`GET /v1/diagnostics`, `/v1/diagnostics/log`;
  credentials in log lines are redacted).
- The agent dialog's tools list is collapsed behind a summary such as
  "7 of 9 tools".
- Gmail (Google's official MCP server) in the connector catalog. A server
  that needs your own OAuth app asks for its client ID and shows **needs your
  OAuth app** or **needs sign-in** until it is set up; the guide also shows
  Gmail through Zapier, without a Google app.
- Agents change the tools and connectors of agents they created
  (`set_agent_tools`): at most their own, and more tools need your approval
  unless Auto mode is on.
- Agents read the user guides (`list_guides`, `read_guide`), so Ferro can walk
  you through a setting.
- Daemon (`ferrobotd`) with a versioned HTTP/WebSocket API (`/v1`), operator
  token on every route, OpenAPI spec and JSON Schemas in `protocol/`.
- One sandboxed computer per agent: Docker (default), local processes, and a
  Firecracker backend that is written but has never booted (needs KVM).
- Sleep levels: warm (pause), cold (stop) and archived (encrypted archive in
  object storage, restored on wake); snapshots and restore for Docker.
- Agents talk through a mailbox with retries, `@name` mentions, question cards
  for the operator, and `create_agent` behind an Approve/Decline card.
- Auto mode (Settings, Agents): agents create teammates without an approval
  card and the chat notes it; the agent limit still applies.
- CLIs (Settings, CLIs; Docker): keyless Debian packages such as jq, pandoc or
  ffmpeg built into one shared sandbox image; agents get it on their next wake.
- Memory per agent and a global memory, routines (cron with time zones),
  shared files and HTML pages behind signed, expiring links.
- Model providers: Anthropic, OpenAI, Gemini, Amazon Bedrock, Google Vertex
  AI, OpenRouter, Ollama, any OpenAI-compatible endpoint, OpenAI Codex and
  Gemini CLI. Keys stay in the OS keychain; sandboxes reach models only
  through the daemon's proxy.
- Runner drivers: Claude Agent SDK, OpenAI-compatible loop, Codex
  app-server, ACP (Gemini CLI) and a mock driver for demos and tests.
- Connectors: remote MCP servers lent to assigned agents, with OAuth sign-in.
- Delegated tasks between agents: `assign_task` and `list_tasks`, an
  automatic report back when a delegate's turn ends and one reminder; mail
  from an agent wakes a sleeping one; new agents greet the operator first.
- Voice: a hands-free call with an agent (replies spoken, your speech
  transcribed live) and dictation in the message box, through ElevenLabs or
  OpenRouter with keys in the keychain; agents can bring teammates into a
  call (`invite_to_call`, at most four agents).
- Model picker: each provider's model list (or suggestions) in a dropdown,
  with a custom id when the list lacks one.
- Agent list: pin, reorder by drag and drop, delete by right-click.
- OpenRouter: prompt caching for Claude models and sticky routing per agent.
- Usage tab: model calls, tokens and estimated cost per provider, model and
  agent, from the model proxy; prices in `config/pricing.toml` with overrides
  in Settings; CSV export.
- Desktop app (Tauri) for macOS (universal .dmg) that starts the daemon itself
  and says so when Docker is missing, and a Windows installer (x64, first
  release, little testing).
- Runner image published as `ghcr.io/byquexo/ferrobot-runner`.

### Security

- Encrypted archives (AES-256-GCM), redacted secrets in `Debug`, model proxy
  allowlist per API family, no redirects, header allowlists both ways, agent
  tokens blanked in committed images.

[Unreleased]: https://github.com/byQuexo/ferrobot/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/byQuexo/ferrobot/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/byQuexo/ferrobot/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/byQuexo/ferrobot/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/byQuexo/ferrobot/releases/tag/v0.1.0
