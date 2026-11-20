# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - unreleased

First public release.

### Added

- Daemon (`ferrobotd`) with a versioned HTTP/WebSocket API (`/v1`), operator
  token on every route, OpenAPI spec and JSON Schemas in `protocol/`.
- One sandboxed computer per agent: Docker (default), local processes, and a
  Firecracker backend that is written but has never booted (needs KVM).
- Sleep levels: warm (pause), cold (stop) and archived (encrypted archive in
  object storage, restored on wake); snapshots and restore for Docker.
- Agents talk through a mailbox with retries, `@name` mentions, question cards
  for the operator, and `create_agent` behind an Approve/Decline card.
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
- Desktop app (Tauri) for macOS (.dmg) and Windows (.exe) that starts the
  daemon itself and says so when Docker is missing.
- Runner image published as `ghcr.io/byquexo/ferrobot-runner`.

### Security

- Encrypted archives (AES-256-GCM), redacted secrets in `Debug`, model proxy
  allowlist per API family, no redirects, header allowlists both ways, agent
  tokens blanked in committed images.

[Unreleased]: https://github.com/byQuexo/ferrobot/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/byQuexo/ferrobot/releases/tag/v0.1.0
