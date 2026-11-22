Two ways in: download the desktop app, or build from source.

## A. Desktop download

Desktop builds come from the repository's Releases page: v0.1.0 has a
universal `.dmg` for macOS. A Windows installer follows in a later release.

1. **Install Docker Desktop** and start it. Every agent runs in a container,
   so it is required. If Docker is missing or not running, the app tells you
   so and links to the download.
2. **Open the app.** It bundles the daemon (`ferrobotd`) and starts it for
   you.
3. **Meet your first agent.** A fresh app greets you with a starter agent. It
   shows whether Docker is ready and asks for a model to think with: add a
   model provider and its key right there. Then it wakes up as your first
   agent, `guide`, and asks what it should do for you. Prefer to set up
   alone? Click **Skip**, add a provider under **Settings → Model providers**
   (its key goes in **Settings → Vault**) and create an agent with
   **New agent**. See [[Model providers and keys|Providers]].
4. **Wait for the first start.** The first time an agent starts, the runner
   image (`ghcr.io/byquexo/ferrobot-runner:0.1.0`, about 2 GB) is pulled
   from the GitHub Container Registry. Later agents start from the cached
   image.

Builds are unsigned unless the release notes say otherwise, so macOS
(Gatekeeper) warns on first launch: right-click the app, choose **Open**, then
confirm. More fixes: [[Troubleshooting]].

The guides under **Settings → Guides** in the app are the same pages as this
wiki.

The desktop window itself has had far less testing than the daemon; most UI
checks ran in a browser against a real daemon.

## B. From source

Requirements: Rust (stable, 1.88 or newer), Node.js 24, and Docker or Podman.
Run everything from the repository root.

### 1. Build the runner image and start the daemon

```bash
docker build -f runner/Dockerfile -t ferrobot-runner:dev .
cargo run -p ferrobot-daemon -- --backend docker --driver mock --agent-listen 0.0.0.0:7879
```

- `--driver mock` uses a fake model: no key, no cost.
- `--agent-listen 0.0.0.0:7879` lets containers reach the agent API through
  `host.docker.internal`. Every request there needs the agent's own token.
- The operator API stays on `127.0.0.1:7878`. Its token is written to
  `.ferrobot/api-token` on first start.

### 2. Create two agents and let them talk

```bash
AUTH="Authorization: Bearer $(cat .ferrobot/api-token)"
curl -s -H "$AUTH" -X POST localhost:7878/v1/agents -H 'content-type: application/json' -d '{"name":"alice"}'
curl -s -H "$AUTH" -X POST localhost:7878/v1/agents -H 'content-type: application/json' -d '{"name":"bob"}'
curl -s -H "$AUTH" -X POST localhost:7878/v1/agents/<alice-id>/messages \
     -H 'content-type: application/json' -d '{"body":"@bob hello"}'
curl -s -H "$AUTH" localhost:7878/v1/agents/<alice-id>/messages
```

With the mock driver, `@name text` makes the agent send `text` to `name`,
`remember key: value` and `recall query` use the memory tools, and an agent
answers mail from another agent with `re: <text>`.

### 3. Optional: a real model with Ollama, no API key

This runs a local model on your machine. It checks that the whole path works
(sandbox, runner, model proxy, tool calls) without any key or cost.

1. Install [Ollama](https://ollama.com) on the same machine as the daemon,
   start it on its default port (11434) and pull a small model:

   ```bash
   ollama pull qwen2.5:3b
   ```

   The tested runs started Ollama with `OLLAMA_CONTEXT_LENGTH=32768`.

2. Start the daemon with `--driver claude` instead of `--driver mock`. That
   flag means "real models": each agent's provider picks its driver. Without
   `ANTHROPIC_API_KEY` it prints a warning; that is fine here.

   ```bash
   cargo run -p ferrobot-daemon -- --backend docker --driver claude --agent-listen 0.0.0.0:7879
   ```

3. Add an Ollama provider and an agent that uses it:

   ```bash
   curl -s -H "$AUTH" -X POST localhost:7878/v1/providers -H 'content-type: application/json' \
        -d '{"name":"ollama","kind":"ollama","default_model":"qwen2.5:3b"}'
   curl -s -H "$AUTH" -X POST localhost:7878/v1/agents -H 'content-type: application/json' \
        -d '{"name":"olly","provider_id":"<provider-id>"}'
   ```

   The `ollama` kind needs no key and defaults to `http://127.0.0.1:11434/v1`.
   The daemon makes that call on your machine, not the sandbox.

4. Send the agent a message as in step 2.

A model this small only checks the plumbing. In tests, 3B and 4B models ran
the tool calls but often failed the actual task. For real work use a larger
model or a cloud provider.

### 4. Optional: the desktop app from source

Needs a running daemon on `127.0.0.1:7878`.

```bash
cd apps/desktop && npm ci
npm run build && FERROBOT_API_TOKEN_FILE=$PWD/../../.ferrobot/api-token npm run tauri dev
npm run dev        # or: the same UI in a browser at http://localhost:1420
```

The app finds the operator token in `FERROBOT_API_TOKEN`, the file named by
`FERROBOT_API_TOKEN_FILE`, or `$FERROBOT_DATA_DIR/api-token`; otherwise it asks
you to paste it once. On Linux, Tauri needs `libwebkit2gtk-4.1-dev` and
`libsoup-3.0-dev`.

To fill a running daemon with demo agents, mail, memory and routines:
`scripts/seed-demo.sh`.

### Without Docker

For development only, with **no isolation**: build the runner and use the
local backend.

```bash
npm --prefix runner ci && npm --prefix runner run build
cargo run -p ferrobot-daemon -- --backend local --driver mock
```

## Cleaning up

`DELETE /v1/agents/{id}` removes an agent with its disk and snapshots
(`?keep_data=true` keeps them). Delete your agents, then stop the daemon;
`docker ps -a` should then list no agent containers.

All flags: `cargo run -p ferrobot-daemon -- --help`.
