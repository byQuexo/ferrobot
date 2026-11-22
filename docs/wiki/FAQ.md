### Why does every agent get its own computer?

Agents run shell commands, install packages and write files. A sandbox per
agent keeps them away from each other and from your machine, gives each one a
disk that persists, and lets an idle agent sleep without touching the others.
The sandbox is the security boundary; the runner's own guard rails are only a
second line.

### Are my API keys safe?

Keys stay in the daemon. They are stored in the OS keychain, and the model
proxy adds them to each request upstream. A sandbox only holds its own agent
token, which works for that agent only and only for an allowlist of model
calls. In tests (fake keys in Docker, and once with a real key) the key was
found in no container env, file or log, nor in the daemon log or data dir.

What remains: an agent can still use the model you gave it, so it can spend
money on your key. Anyone with the operator token can run commands in any
agent's computer. `--secret-store file` keeps keys in plaintext and is meant
for CI only.

### Which models can I use?

Anthropic, OpenAI, Google Gemini, Claude on Amazon Bedrock and Google Vertex
AI, OpenRouter, Ollama, any OpenAI-compatible endpoint, OpenAI's Codex
harness (API key or ChatGPT sign-in) and Google's Gemini CLI. Most of them
have only run against fake upstreams so far. Claude subscription logins are
not supported. See [[Providers]].

### Does it work offline with Ollama?

Model calls can stay on your machine: the daemon calls Ollama on the same
computer, and no key is needed. Building or pulling the runner image and
pulling the model need the internet once. A fully offline run has not been
tested. Small local models are good for checking the plumbing, not for real
agent work.

### Does it run on Windows and Linux?

- **Linux:** the daemon is built and tested on Linux in CI, including the
  Docker backend. Build the desktop app from source.
- **macOS:** the main development platform, with Docker Desktop.
- **Windows:** v0.1.0 has an installer (x64). It is new and has had little
  testing so far.

### Why Docker?

It runs on macOS, Windows and Linux, and it is the only backend that has been
tested end to end. A Podman socket can be used instead. Firecracker microVMs
would isolate agents more strongly, but they need Linux with KVM and are not
ready.

### What is the status of Firecracker?

The backend is written but **no microVM has ever booted**: it was built on a
machine without KVM and tested against fakes. The jailer and snapshot-based
sleep are not built. See [[Sandbox Backends|Sandbox-Backends]] and
[[Roadmap]].

### What does it cost?

The software is free (MIT or Apache-2.0, your choice). You pay your model
provider for what agents use. Ollama and the mock driver cost nothing. Idle
agents are paused and later stopped, so they use few resources while nothing
happens. The
[[Usage tab|Usage-and-Costs]] (v0.1.0) shows tokens and estimated costs.

### Does it send telemetry?

The daemon, the runner and the desktop app send none. The harnesses inside a
sandbox (Claude Agent SDK, Codex, Gemini CLI) are third-party programs;
their own settings apply, and nothing here changes them.

### Do I need the desktop app?

No. The app is a thin client over the daemon's `/v1` HTTP and WebSocket API.
`curl`, scripts or your own client work the same way; the OpenAPI spec and
JSON Schemas in `protocol/` describe everything.

### How can I contribute?

- Read [[Conventions]]. Every CI check must pass before a merge.
- Most wanted: running the Firecracker backend on a KVM host, and live runs
  with real provider keys. See [[Roadmap]].
- Contributions are dual-licensed under MIT and Apache-2.0, like the project.
