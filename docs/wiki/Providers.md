A provider says where an agent's model requests go and how they are
authenticated. Each agent picks a provider and a model in its profile.

## Adding a provider

- In the desktop app: **Settings → Model providers → Add provider**. Pick the
  kind, give it a name and optionally a default model. Then add its key in
  **Settings → Vault** (the provider's row links there: "add its key in
  Vault"). Vault holds every key in one place: model providers, connectors
  and voice.
- Over the API: `POST /v1/providers` with `name`, `kind` and, depending on
  the kind, `api_key`, `base_url`, `region`, `project` and `default_model`.
  `api_key` is write-only; responses only say `has_key`.

Where to get a key or set a kind up (the app links these too, under the kind
and next to a missing key in Vault):

| Kind | Key or setup |
| --- | --- |
| Anthropic | [Create an API key](https://platform.claude.com/settings/keys) |
| OpenAI | [Create an API key](https://platform.openai.com/api-keys) |
| Google Gemini, Gemini CLI | [Create a Gemini API key](https://aistudio.google.com/apikey) |
| Amazon Bedrock | [Create a Bedrock API key](https://docs.aws.amazon.com/bedrock/latest/userguide/api-keys.html) |
| Google Vertex AI | [Set up Claude on Vertex AI](https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/claude) |
| OpenRouter | [Create an API key](https://openrouter.ai/settings/keys) |
| Ollama | [Download Ollama](https://ollama.com/download); no key |
| OpenAI Codex | [An OpenAI API key](https://platform.openai.com/api-keys), or a ChatGPT sign-in with [Codex installed](https://developers.openai.com/codex/cli) (see below) |
| Claude Code (your own login) | No key: [log in to Claude Code](https://code.claude.com/docs/en/authentication) yourself (see below) |

Then set `provider_id` and `model` on an agent (`PATCH /v1/agents/{id}` or the
agent dialog). Without a model the agent uses the provider's
`default_model`. Agents without a provider use the daemon's own
`ANTHROPIC_API_KEY` environment variable.

The daemon must run with `--driver claude` ("real models"). With
`--driver mock` every agent runs the mock driver, whatever its provider.

Good to know:

- Changing an agent's provider or model rebuilds its sandbox; the disk is
  kept.
- A provider that agents still use cannot be deleted (`409`).
- Agents created by an agent inherit its provider and model.

## How keys are kept

- Keys are stored in the OS keychain of the daemon's computer (macOS
  Keychain, Windows Credential Manager, Secret Service on Linux), never in
  SQLite and never in a sandbox. `--secret-store file` writes a plaintext
  `secrets.json` (mode 0600) for headless CI only.
- Without a usable keychain the daemon still starts and warns. Storing a key
  then fails instead of falling back to plaintext.
- A sandbox only gets its agent token, in the place the harness expects a
  key. The [[model proxy|Architecture]] checks that token, drops it and adds
  the real credential per request.
- The keychain path has run once, on macOS. Windows Credential Manager and
  Linux Secret Service have never been exercised.

## Provider kinds

| `kind` | Driver | Credential | Sent upstream as | Default upstream |
| --- | --- | --- | --- | --- |
| `anthropic` | `claude` | API key (required) | `x-api-key` | `https://api.anthropic.com` |
| `openai` | `openai` | API key (required) | `Authorization: Bearer` | `https://api.openai.com/v1` |
| `gemini` | `openai` | API key (required) | `Authorization: Bearer` | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `bedrock` | `claude` | Bedrock API key (required) | `Authorization: Bearer` | `https://bedrock-runtime.<region>.amazonaws.com` |
| `vertex` | `claude` | none stored: the daemon host's `gcloud` login | `Authorization: Bearer <gcloud token>` | `https://<region>-aiplatform.googleapis.com/v1` |
| `openrouter` | `openai` | API key (required) | `Authorization: Bearer` | `https://openrouter.ai/api/v1` |
| `ollama` | `openai` | none needed | `Authorization: Bearer` if a key is set | `http://127.0.0.1:11434/v1` |
| `openai_compatible` | `openai` | API key (optional) | `Authorization: Bearer` if set | none: `base_url` is required |
| `codex` | `codex` | OpenAI API key, or a ChatGPT sign-in on the daemon host | `Authorization: Bearer` | `https://api.openai.com/v1` |
| `gemini_cli` | `acp` | Gemini API key (required) | `x-goog-api-key` | `https://generativelanguage.googleapis.com` |
| `claude_code` | `claude_code` | none: your own `claude` login on the daemon's computer | not proxied | none: the host's `claude` CLI |

`base_url` overrides the default upstream for any kind. The upstream is
called by the daemon, so `127.0.0.1` in it means the daemon's computer, not
the sandbox.

Notes per kind:

- **Bedrock** needs `region` and a Bedrock API key. IAM credentials and
  SigV4 signing are not supported.
- **Vertex** needs `region` (`global` works) and `project`. The proxy runs
  `gcloud auth print-access-token` on the daemon host; there is no
  service-account key handling.
- **Gemini** (`gemini`) uses Google's OpenAI-compatible endpoint with the
  runner's own tool loop. **Gemini CLI** (`gemini_cli`) runs Google's Gemini
  CLI harness with its own tools instead.
- **Codex** runs OpenAI's Codex harness (`codex app-server`) inside the
  sandbox. With an API key the proxy forwards to OpenAI. Without one it uses a
  ChatGPT sign-in done on the daemon's computer: "Sign in with ChatGPT"
  shows a device code, the host's `codex` CLI (`--codex-bin`) does the
  sign-in, the tokens go to the keychain and the proxy adds them. They never
  enter a sandbox.

  The sign-in needs the Codex CLI on the daemon's computer; an API key does
  not. Install it, then press **Sign in with ChatGPT** again:
  - macOS or Linux: `curl -fsSL https://chatgpt.com/codex/install.sh | sh`,
    or `brew install --cask codex`
  - Windows, in a new PowerShell window:
    `powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"`
  - Anywhere with Node.js: `npm install -g @openai/codex`

  The daemon looks on PATH and where these installers put Codex
  (`~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`; on Windows
  `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin` and npm's `%APPDATA%\npm`), so
  the app finds it without a restart. Elsewhere, point `--codex-bin` at it.
  Until a Codex provider has a key or a sign-in, the first-run setup asks for
  one before Ferro wakes.
- **Ollama** needs no key. Ollama must listen on the daemon's computer (or
  set `base_url`).
- **Claude Code (your own login)**: see the next section.
- A model that can't use tools (some OpenRouter and Ollama models) holds the
  agent's messages and asks you whether to run the agent without tools; see
  [[Troubleshooting]].

## Claude Code (your own login)

Opt-in: nothing uses it until you add a provider of this kind. ferrobot runs
the daemon computer's own, unmodified `claude` CLI (`claude -p`), signed in by
you with `claude auth login`, Anthropic's own flow. ferrobot never reads,
stores or relays that login: no token in Vault, the keychain or the model
proxy, and no Claude sign-in in the app.

- **Your seat:** uses your own Claude subscription seat. All agents share
  your seat's limits. Do not share your seat. Anthropic's terms: "Advertised
  usage limits for Pro, Max, and Ultra plans assume ordinary, individual usage
  of Claude Code and the Agent SDK." Signing in to your own copy is allowed:
  the restrictions do not "prevent an end user from signing in to the
  unmodified Claude Code binary with their own Claude subscription". See
  [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance).
- **Host, not sandbox:** `claude` and its login stay on the daemon's
  computer. It runs with its built-in tools off and none of your settings,
  hooks, plugins, CLAUDE.md or claude.ai connectors (only the hooks and
  plugins you set for ferrobot, below). It reaches the agent's
  computer only through ferrobot's MCP tools `bash`, `read_file` and
  `write_file`, which run inside the sandbox, next to the usual ferrobot
  tools. The runner's check for destructive shell commands does not apply on
  this path; the sandbox still contains them. `claude` itself would attach a
  file named as `@path` and run a leading `/command` on the host, so the
  daemon puts a space after such an `@` and refuses a prompt starting with
  `/` or `!`.
- **Usage:** the calls go from your `claude` to Anthropic, past the model
  proxy. The daemon reads their tokens from `claude`'s own output, so Usage
  shows them, with a cost estimate at API prices (your seat is not billed
  per token).
- **Settings** shows whether `claude` is installed and logged in, with
  **Check again** after you log in in a terminal.
- **Sessions:** each agent works in its own folder in the data dir
  (`claude-code/<agent id>`), so its conversation also shows up in your
  `~/.claude/projects`. Deleting the agent removes the folder; Claude Code
  keeps its transcripts.
- One turn at a time per agent; messages that arrive during a turn wait for
  the next one. `--claude-bin` sets the CLI's path; the daemon also looks
  where its installer puts it (`~/.local/bin`, on Windows
  `%USERPROFILE%\.local\bin`).

### Hooks and plugins

**Settings → Claude Code** sets hooks and plugins for every agent on Claude
Code. They are ferrobot's own: the daemon writes a settings file with only
these hooks for each turn (`--settings`) and loads each plugin you switch on
from its install folder (`--plugin-dir`). Your own `~/.claude` settings stay
out, and agents cannot change either.

- **Hooks** are shell commands that run on this computer, outside the
  sandboxes, when an event fires: before or after a tool call (optionally only
  for some tools, e.g. `mcp__ferrobot__bash`), when a turn starts or ends, or
  when a session starts or ends. A hook gets the event as JSON on stdin;
  exit code 2 before a tool call blocks it. See Claude Code's
  [hooks reference](https://code.claude.com/docs/en/hooks).
- **Plugins** are the ones installed in your Claude Code
  (`claude plugin install ...`). Their hooks run. Their skills, commands and
  subagents need Claude Code's built-in tools, which stay off. Their MCP
  servers do not start; add one under Connectors as a local server, which
  shows you its command first.
- `claude`, its hooks and plugins get the daemon's environment without
  Anthropic and Claude Code variables, the operator token (`FERROBOT_*`) or
  storage keys (`AWS_*`).
- If the settings cannot be read, or Claude Code cannot list its plugins
  while one is on, the turn does not start rather than run without them.

## Voice providers

Voice (calls and dictation) uses its own providers, separate from the model
providers: ElevenLabs (speech and live transcription) or OpenRouter (speech
and transcription models). Pick one under **Settings → Voice**; its key goes
in **Settings → Vault** and stays in the daemon's secret store like model
keys. See [[Voice and calls|Voice-and-Calls]].

## Not supported

- **A Claude sign-in inside ferrobot.** ferrobot never handles Claude
  credentials. To use your subscription, log in to your own Claude Code and
  add a Claude Code (your own login) provider (above).
- Bedrock with IAM credentials or SigV4.

## What has been tested

Most kinds have never run against their real upstream. Every kind's
credential handling is tested against a fake upstream, including a check
that no key reaches the sandbox env, files or logs.

| Kind | Real upstream run? |
| --- | --- |
| `anthropic` | Yes, once: a tool-using turn with a real key from the macOS Keychain. |
| `ollama` | Yes: small local models on a laptop. The plumbing worked every time; `qwen2.5:3b` passed the test task once and failed its answer check in two other runs. |
| `openai_compatible` | Yes, against Ollama. |
| `codex` | Only against Ollama's Responses API. No OpenAI key used; a real ChatGPT sign-in was never tried, and whether ChatGPT's backend accepts these requests is unverified. |
| `gemini_cli` | No: a fake Gemini API only. |
| `claude_code` | Yes, on macOS with Claude Code 2.1.330 and a claude.ai login: greeting, `bash`, `write_file`, `read_file` and `remember` in a Docker sandbox, a resumed session and a stopped turn. Windows never. |
| `openai`, `gemini`, `openrouter` | No: fake upstreams only. Gemini's endpoint with these tool schemas is untested. |
| `bedrock`, `vertex` | No: request shapes were checked by capturing the Claude SDK's requests against a local server. No AWS or GCP call. |

The provider dialogs were checked in a browser against a real daemon, not in
the desktop window.
