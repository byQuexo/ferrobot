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
- **Ollama** needs no key. Ollama must listen on the daemon's computer (or
  set `base_url`).
- A model that can't use tools (some OpenRouter and Ollama models) holds the
  agent's messages and asks you whether to run the agent without tools; see
  [[Troubleshooting]].

## Voice providers

Voice (calls and dictation) uses its own providers, separate from the model
providers: ElevenLabs (speech and live transcription) or OpenRouter (speech
and transcription models). Pick one under **Settings → Voice**; its key goes
in **Settings → Vault** and stays in the daemon's secret store like model
keys. See [[Voice and calls|Voice-and-Calls]].

## Not supported

- **Claude subscription logins** (signing in with a Claude account instead of
  an API key). This is by design: Claude models are used with API keys or
  through Bedrock and Vertex only. The ChatGPT sign-in for Codex is a separate
  feature and is supported.
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
| `openai`, `gemini`, `openrouter` | No: fake upstreams only. Gemini's endpoint with these tool schemas is untested. |
| `bedrock`, `vertex` | No: request shapes were checked by capturing the Claude SDK's requests against a local server. No AWS or GCP call. |

The provider dialogs were checked in a browser against a real daemon, not in
the desktop window.
