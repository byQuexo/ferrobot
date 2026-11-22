Common problems and how to fix them.

## Docker is missing or not running

Every agent works in its own Docker container, so the desktop app needs
Docker Desktop.

- If Docker is not running when the app starts, the app shows a page that
  says so, with a link to download Docker Desktop. Install or start Docker and
  wait: the page checks every few seconds and continues by itself.
- If Docker stops later, agents cannot start. Start Docker again; the daemon
  reconnects without a restart.
- The daemon reports the state in `/v1/health`: `sandbox_ready` is `false`
  and `sandbox_error` says why.
- A Podman socket can stand in for Docker.

## "Key missing" or "Key rejected" on an agent

The status under an agent's name shows a problem with its model credential.
Its messages wait until it is fixed; nothing is lost.

- **Key missing**: the agent's provider has no key, or the agent has no
  provider and the daemon has no Anthropic key.
- **Key rejected**: the provider refused the key (HTTP 401), for example
  because it is wrong or revoked.

How to fix it:

- Click **Open Vault** next to the status and add or replace the provider's
  key. For an agent without a provider, click **Choose a model** and pick a
  provider in its dialog. See [[Model providers and keys|Providers]].
- Storing a key or changing the agent's provider or model clears the problem
  and the waiting messages run.
- Fixed it somewhere else? Click **Retry**. If the key still does not work,
  the next model call shows the problem again.

## An agent never answers and its model can't use tools

Agents on the `openai` driver (OpenAI, Gemini, OpenRouter, Ollama and
OpenAI-compatible providers) send their tools with each model request. Some
models have no tool support, for example OpenRouter models without an
endpoint that takes tools ("No endpoints found that support tool use") or
Ollama models that "do not support tools". The status under the agent's name
then says
**Model can't use tools**, its messages wait, and a card in its chat asks:

- **Use without tools**: the agent loses its tools and connectors and its
  waiting messages run. It can then only chat: no shell, files, messages to
  other agents, memory or connectors. Give tools back in its dialog.
- **Keep tools**: nothing changes and messages keep waiting. Click
  **Choose a model** next to the status and pick a model with tool support;
  changing the model or editing the provider clears the problem.

## The first agent takes long to start

The first time an agent starts, Docker pulls the runner image
(`ghcr.io/byquexo/ferrobot-runner`, about 2 GB) from the GitHub Container
Registry. Depending on your connection this takes a few minutes. Later agents
start from the cached image.

- The pull needs the internet once. If it fails, the error names the image;
  check your connection and try again.
- After building CLIs, agents switch to the new image when they next start or
  wake. See [[CLIs]].

## macOS says the app cannot be opened

Builds are unsigned unless the release notes say otherwise, so macOS
(Gatekeeper) warns on the first launch. Right-click the app, choose **Open**,
then confirm. You only need to do this once.

## The app cannot reach the daemon

The app shows "Can't reach the ferrobot daemon" and retries by itself. The
desktop download starts its bundled daemon; a daemon built from source must
be started with `ferrobotd`. If it runs on another address, enter it on that
page.

If the app asks for its access token, paste the content of `api-token` from
the daemon's data folder.

## Diagnostics and logs

**Settings → Diagnostics** shows the app and daemon versions, whether Docker
is ready, the runner image, how many agents, providers and connectors you
have, the data folder, and the daemon's latest log lines.

- **Copy diagnostics** copies all of it as plain text, with the last 200 log
  lines. Paste it into a bug report. Keys and tokens are never part of it, and
  anything in the log that looks like one reads `[REDACTED]`.
- **Copy path** copies the data folder's path. **Refresh** reloads the log.
- The log there is kept in memory since the daemon started. The desktop app
  also writes the daemon's whole log to `daemon.log` in its data folder, new
  on each start:
  - macOS: `~/Library/Application Support/dev.ferrobot.desktop/`
  - Windows: `%APPDATA%\dev.ferrobot.desktop\`
- To remove all data, quit the app, delete that folder and remove the Docker
  containers and volumes whose names start with `ferrobot-`. Keys stay in the
  system keychain under `ferrobot` until you delete them there.
