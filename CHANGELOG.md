# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.9] - 2026-11-24

### Added

- Pins per chat: a chat with pins shows a pin with their number in its header;
  it lists that chat's pinned replies and files (the global pin list stays).
- An agent's details panel lists its webhooks: the incoming ones that message
  it and the outgoing ones that report on it.
- Long instructions in an agent's details fold to their first six lines, with
  **Show all**, so the cards below stay in view.
- The Discord bridge reacts to a message with 👀 once it reached the agent and
  swaps it for ✅ when the reply is posted (the bot needs **Add Reactions**).
  In the chat, a message from Discord shows a chip with who wrote it instead
  of the "via Discord" line.

### Changed

- Settings → Webhooks is redesigned: each webhook is a card with its status,
  destination, agents and events, and a new one is added in a dialog with a
  hint per destination, events and agents as chips, and its own Add button.
- Settings → Discord says why the bridge left a message out: a user not in the
  list, or an empty message because Message Content Intent is off.
- The roadmap has Flows: set-up series of steps that a trigger or an agent
  starts. Voice over Discord is on the ideas list.

### Fixed

- Settings' section list ran past the bottom of the window at the default
  size; it is more compact now and scrolls when it still does not fit.

## [0.1.8] - 2026-11-24

### Added

- Dashboards: **Use as dashboard** next to any shared `.html` file keeps it
  under **Dashboards** in the sidebar. It opens in the main area with a tab
  per dashboard, full screen or in your browser, and always shows the newest
  version the agent shared of that file (`/v1/dashboards`).
- Traces show the cards a turn waited on (from the card to your answer, with
  approved, declined, answered or timed out) and the messages it handed to
  other agents. Click a handoff to open the turn that took it, and Back to
  return; a turn started by another agent links to the turn that sent it, so
  a task's answer leads back to the task. The OpenTelemetry export sends
  waits as spans and handoffs as span links, without message text.
- Webhooks (Settings → Webhooks, guide: Webhooks). Incoming: a URL per
  webhook, shown once, whose POSTs (JSON or text, up to 64 KiB) become
  messages to an agent, marked as external data and not instructions. The
  secret lives only in the URL; New URL replaces it. Outgoing: replies, cards
  waiting for you, finished tasks and failed turns go to a Discord or Slack
  channel or any URL as JSON, optionally for some agents only, with the last
  delivery's result in the list. Their URLs are kept in the keychain.

- Discord bridge (Settings → Discord, guide: Discord bridge): in a bridged
  channel, messages from the Discord users you allow reach that channel's
  agent as yours ("via Discord"), and its reply comes back as a reply. The
  daemon reads the channel through Discord's API with your bot's token (kept
  in the keychain), so nothing needs to be reachable from the internet
  (`/v1/discord`, `/v1/discord/token`).

### Changed

- Auto mode keeps a card for the riskiest steps: connector tools that move
  money or delete or cancel something, force-pushing and publishing a package
  now ask even in Auto mode ("a very risky step"). Everything else risky still
  runs without a card, and dangerous steps stay refused.
- Roadmap: the ideas list moved onto the roadmap (the Computer tab's file
  browser and terminal, logins for CLIs and finer connector scopes, faster
  calls, a stable signature for development builds).

### Fixed

- A mention of a deleted agent showed `@unknown-agent`; it now shows its last
  name, `@calc-a (deleted)`, in the chat, in exchange lines and in what other
  agents read. The daemon keeps the name when an agent is deleted, and finds
  it in the history for agents deleted before (`/v1/agents/former`).

## [0.1.7] - 2026-11-24

### Added

- A download button next to every file an agent shares in the chat, HTML
  pages included (they open in the browser when clicked, so there was no way
  to save them). It saves through your browser, as the file viewer does.
- Pins: a pin button on every reply and shared file, and a pin button next to
  Settings that lists pinned agents, replies and files. Click a reply to jump
  to it in its chat (it flashes), or a file to open it; rename pins, drag to
  reorder, unpin. Pins live in the daemon (`/v1/pins`); agents keep their own
  pin (top of the sidebar). The sidebar header's buttons got a little
  narrower so the name still fits.

### Fixed

- Agents learn of a rename: the next turn of every agent that knew the old
  name starts with a note ("@scout is now @scout-x"), the renamed agent hears
  its new name, and its chat shows "Renamed from @scout to @scout-x".
- Old names from renames before 0.1.6, which were not recorded, are found at
  start in the history (the names agents sent messages under and were created
  with), so `@old-name` reaches the renamed agent again and agents are told
  its new name. Text already sent keeps the old name as plain text.

## [0.1.6] - 2026-11-24

### Added

- Traces: a reply's steps (in its footer, by the reaction button) open its
  turn on a timeline: model calls (from the proxy, now tagged with their turn,
  or for Claude Code from its own stream) and tool calls with how long each
  took, the slowest one, failures, counts and a search
  (`GET /v1/agents/{id}/turns/{turn_id}/trace`). Computer steps name their
  command or file. New migration: v20 (`usage.turn_id`, `usage.at_ms`).
- Settings → Traces: each finished turn's trace goes to Phoenix, Langfuse or
  any OpenTelemetry backend over OTLP/HTTP (protobuf, OpenInference and GenAI
  attributes), with presets, keys in the keychain and the last export's
  status. Off by default; tool inputs, tool output and replies only with
  **Include content** (`/v1/otel`, `/v1/otel/secret`). Guide: Traces.
- Settings → Claude Code: hooks and plugins for agents on Claude Code (your
  own login). Hooks are commands that run on this computer before or after a
  tool call, when a turn or session starts or ends; installed plugins can be
  switched on (their hooks run; their MCP servers go through Connectors), and
  a Subagents switch turns on Claude Code's own subagents (`Task`), which get
  only ferrobot's tools. ferrobot passes only these (`--settings`,
  `--plugin-dir`); your own Claude Code settings stay out, and agents cannot
  change them (`/v1/claude-code/settings`, `/v1/claude-code/plugins`).
- Memory in an Obsidian vault: in Settings → Global memory, pick your vault's
  folder and every memory (agents' own and global) becomes a Markdown note
  under `ferrobot/` that you can read and edit in Obsidian; stored memories
  are copied in (`/v1/memory-vault`).
- Local MCP servers: a connector can be a command (for example an `npx` server)
  that ferrobot starts on this computer when it starts, without a shell and
  with a cleared environment; its token goes in as the environment variable you
  name. Agents can propose one with `add_connector` (`command`); the card shows
  the exact command and Auto mode never skips it. New migration: v22
  (`connectors.command`, `connectors.token_env`).
- Compact now: in the context popover, Claude Code agents can compact their
  conversation (`POST /v1/agents/{id}/compact`). The daemon runs `claude`'s
  own `/compact` on the agent's session, with no tools and none of your
  settings; message text can still never run a slash command.
- Claude Code calls show on the Usage page: each model call `claude` reports is
  a usage row (provider kind `claude_code`) with its tokens and a cost
  estimated at Anthropic API prices; your subscription covers them.
- Call volume: a speaker button in the call bar opens a small slider for how
  loud the agents speak (0 mutes them); this computer remembers it.
- Update notice: the sidebar corner says when a new release is out (desktop
  app).

### Changed

- Calls with several agents: an agent without a voice of its own speaks with an
  `ElevenLabs` voice from your account that no one else in the call uses (by
  its place in the call); the host keeps the default.
- Settings → Voice is one flow in the new card look: pick the provider, paste
  its key right there, and your voices load at once. Every change saves by
  itself (no Save button); switching providers asks first when it would clear
  your voice picks. The chat's voice popover got the same look.
- Settings → Connectors: your connectors are cards with a tile, their status
  (connected, needs sign-in, needs a token, failed), the agents that use them
  and the next step (**Sign in**, **Add its token in Vault**, which opens
  Vault at that token, **Check again**). The catalog is grouped (Work & notes,
  Code & cloud, Data & payments, Docs & search, Mail & automation) with a
  search, and **Add a custom connector** opens a dialog.
- Usage: plan limits are labelled bars ("5-hour window", "Weekly") with the
  percentage used and the reset time, plus loading and error states with
  **Try again**. Claude Code costs are noted as estimates at API prices,
  covered by your subscription.
- `claude` (Claude Code agents) no longer gets the daemon's `FERROBOT_*` and
  `AWS_*` variables, so hooks and plugins never see the operator token or
  storage keys.

### Fixed

- A renamed agent can still be reached by its old name: agents that write
  `@old-name` (or send to it) reach it, until another agent takes that name.
  New migration: v21 (`agent_aliases`).
- The chat's voice popover stayed open on top of dialogs such as Edit agent;
  it now closes when a dialog opens, and dialogs always sit above popovers.

## [0.1.5] - 2026-11-23

### Fixed

- A message an agent was already working on still showed as "queued" with
  **Send now**, which would have stopped that very turn: the daemon now tells
  the app when a turn takes a message.

## [0.1.4] - 2026-11-23

### Added

- Call bar: the speaking speed (ElevenLabs; click steps 0.7×–1.2×, the same
  speed setting as in Settings) and push-to-talk, a toggle with a hold-to-talk
  button and Space, remembered on this computer.

- Agent tags, a model switch in the chat header and a context meter. Up to five
  tags per agent show next to its name (`tags` on agents, migration v18). The
  chat header's model chip switches the model (the agent's computer restarts).
  A ring by the message box shows how full the agent's context is
  (`GET /v1/agents/{id}/context`, from its newest model call; for Claude Code
  from the token counts `claude` reports in its stream) and offers New chat (`POST /v1/agents/{id}/new-session`): the
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
- In a group chat, `@` opens a picker of its members; the mention becomes a
  chip and only that agent answers. A line shows who is writing, and agent
  names take their avatar's colour.
- You can react to an agent's message with the smiley button; the agent is
  told which reply got which emoji.
- Each chat keeps its unsent draft when you switch to another chat or to
  Settings.
- Settings groups its pages under categories, with Appearance first.
- Files, reactions and subagents in the chat: attach files with the
  paperclip, drag and drop or paste (`POST /v1/agents/{id}/uploads` puts them
  in the agent's `uploads/` folder, at most 25 MB each); agents react to your
  messages with one emoji (`react` tool, `reaction` event); OpenAI-compatible
  agents get a `subagent` tool that runs tasks in parallel with the same tools.
- Send now: ⌘↵ (Ctrl+Enter) sends what you typed ahead of a running turn,
  and queued messages get a **Send now** link. The daemon stops the turn only
  when the agent's runner cannot take messages mid-turn (`now` on
  `POST /v1/agents/{id}/messages`); listed under Settings → Keyboard.

### Changed

- An agent's `add_connector` card no longer asks for the token: approving it
  adds the connector and gives it to the agent, and you put its token in
  Settings → Vault. No card asks for a key.
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
- The voice call moves from the sidebar to a bar at the top of the window, so
  it stays in view on every page: every agent in the call, a microphone level
  meter, the call's length, and Open chat, Mute and Hang up.

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

[Unreleased]: https://github.com/byQuexo/ferrobot/compare/v0.1.9...HEAD
[0.1.9]: https://github.com/byQuexo/ferrobot/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/byQuexo/ferrobot/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/byQuexo/ferrobot/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/byQuexo/ferrobot/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/byQuexo/ferrobot/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/byQuexo/ferrobot/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/byQuexo/ferrobot/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/byQuexo/ferrobot/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/byQuexo/ferrobot/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/byQuexo/ferrobot/releases/tag/v0.1.0
