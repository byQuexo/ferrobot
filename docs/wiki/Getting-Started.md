Two ways in: download the desktop app, or build from source.

## A. Desktop download

Desktop builds come from the repository's Releases page: v0.1.11 has a
universal `.dmg` for macOS and an `.exe` installer for Windows (x64).

1. **Install Docker Desktop** and start it. Every agent runs in a container,
   so it is required. If Docker is missing or not running, the app tells you
   so and links to the download.
2. **Open the app.** It bundles the daemon (`ferrobotd`) and starts it for
   you.
3. **Meet Ferro, your main agent.** A fresh app greets you with Ferro. It
   shows whether Docker is ready and asks for a model to think with: add a
   model provider and its key right there. Then Ferro (`ferro`, pinned at the
   top) wakes up and asks what it should do for you. It stays your main agent:
   give it tasks, and it builds a team of agents when a job needs one. Prefer to set up
   alone? Click **Skip**, add a provider under **Settings → Model providers**
   (its key goes in **Settings → Vault**) and create an agent with
   **New agent**. See [[Model providers and keys|Providers]].
4. **Wait for the first start.** As soon as Docker runs, the daemon
   downloads the runner image (`ghcr.io/byquexo/ferrobot-runner:0.1.11`,
   about 2 GB) from the GitHub Container Registry in the background; an agent
   that starts before it is done waits for it. Later agents start from the
   cached image, and an update downloads only the parts that changed.

Builds are unsigned unless the release notes say otherwise, so the system
warns on first launch:

- **macOS** (Gatekeeper): right-click the app, choose **Open**, then confirm.
- **Windows** (SmartScreen): click **More info**, then **Run anyway**.

More fixes: [[Troubleshooting]].

The guides under **Settings → Guides** in the app are the same pages as this
wiki.

### Keyboard shortcuts

⌘ on macOS, Ctrl on Windows and Linux. **Settings → Keyboard** lists them too.

- **⌘K** go to an agent or a settings page
- **⌘N** new agent · **⌘,** settings · **⌘⇧U** usage
- **⌘I** show or hide the details panel · **⌘B** collapse or expand the sidebar
- **⌘1–9** the 1st to 9th agent in the sidebar
- **Esc** close the open dialog, menu, page or panel · **⌘/** all shortcuts

How the app looks (theme, accent color, bubbles, chat background, a pattern
behind the chat, text size, font, corners, density, chat width and motion) is
under **Settings → Appearance**, with a preview at the top.

### Stop

While an agent works, the Send button in its message box turns into **Stop**. It stops the agent and
every agent working on something it handed out (a task, a message, and so on
down the line), and drops the messages agents queued for each other there, so
a loop between agents ends too. Your own queued messages, routines and
webhooks stay; a stopped agent's task does not report back to an agent you
stopped.

### Pins

Keep important things one click away. Hover a reply or a shared file and
click its pin; pin an agent with the pin on its row in the sidebar (it also
moves to the top). The pin button next to Settings lists everything pinned:
click a reply to jump to it in its chat, or a file to open it. Rename a pin
with the pencil, drag to reorder, and unpin with the cross. A chat with pins
also shows a pin with their number in its header: it lists that chat's pinned
replies and files. Pins live in the daemon, so every app that connects to it
sees them.

### Dashboards

Keep HTML pages your agents make (a finance overview, a project board) one
click away. Next to a shared `.html` file, click **Use as dashboard**. It
appears under **Dashboards** in the sidebar; click it to open it in the main
area, with a tab for each dashboard. A dashboard follows the file, not one
copy of it: when the agent shares the same file again (for example from a
morning routine), the dashboard shows the new version. Rename it with the
pencil (or double-click its tab), open it full screen or in your browser,
remove it with the trash can. Dashboards live in the daemon too.

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
