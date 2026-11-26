What comes next, then the status of every phase so far. The detailed log of
what was built and tested is [docs/progress.md](../blob/main/docs/progress.md);
the assumptions behind it are in
[docs/decisions.md](../blob/main/docs/decisions.md).

## What's next

In rough order. Nothing here has a date.

1. **Firecracker on real hardware.** Boot the written backend on a Linux host
   with KVM (F0 to F2), then start it through the jailer (F3), add cold sleep
   from a snapshot (F4b) and a KVM test runner in CI (F5). Docker stays the
   default, and the only choice on macOS and Windows.
2. **Live runs for every provider.** OpenAI, Gemini, Amazon Bedrock and Google
   Vertex AI have only run against fakes, and Codex has never used a real
   ChatGPT sign-in. Each gets a real turn, and fixes where the real API
   differs.
3. **Windows and updates.** More testing of the Windows app (installer,
   Credential Manager, Codex), and a release with signed update files, so the
   in-app updater runs against a real release for the first time.
4. **Computer use.** Each agent's computer gets a screen: a virtual desktop
   with a browser inside the sandbox, tools to look at it and click and type,
   screenshots in the chat's step trail, and a live view in the Computer tab.
   Like every other tool it stays inside the sandbox, goes through the tool
   groups (a new `computer` group, off by default) and the risky-step
   classifier, and works with any model that accepts images.
5. **Traces.** A trace view behind each reply's steps line: one timeline of
   the turn's model calls, tool calls, approval waits and the work it handed
   to other agents, with duration, counts and errors. The same traces go out
   over OpenTelemetry (OTLP, GenAI conventions) to an endpoint you choose,
   such as Langfuse, Phoenix, Grafana Tempo or Jaeger. Export is off by
   default; its keys stay in the keychain, and prompts and replies are left
   out unless you turn them on. **Done** in 0.1.8: the trace view with model
   and tool calls, approval waits and handoffs to other agents' turns, and
   the export ([[Traces]]).
6. **Webhooks.** Connect ferrobot to other services both ways. Incoming:
   each agent (or routine) can get its own webhook URL with a secret, so a
   POST from GitHub, a form, n8n or Zapier becomes a message to the agent or
   starts a routine, with the payload attached. Outgoing: events you pick (a
   reply, a question card waiting for you, a finished task, a failed turn) go
   to a URL you choose, such as a Discord or Slack channel. A Discord bridge
   on top lets you write to your agents from a channel and get their answers
   there. Off by default; secrets in the keychain, payloads signed, and no
   key or operator token in anything sent. **Done in 0.1.8:** incoming
   webhooks to an agent (secret in the URL, only its hash stored) and
   outgoing webhooks for those four events in Discord, Slack or JSON format,
   URLs in the keychain ([[Webhooks]]), and the Discord bridge: allowed users
   write to an agent from a channel and get its reply there ([[Discord]]);
   **Done since:** agents post to their channel on their own
   (`send_to_discord`, 0.1.10), and cards are answered from Discord, approvals
   with ✅ or ❌ and questions by reply (0.1.10); **open:** starting a routine
   from a webhook, and signed outgoing payloads.
7. **The agent's computer in the app.** A file browser and a terminal in the
   agent's details panel (Computer tab), so you can look around and fix
   things in its sandbox without asking the agent. **Done** in 0.1.11: a
   file browser (read-only) and a console that runs one command at a time,
   up to 60 s each; full-screen programs like vim need a real terminal.
8. **Logins for CLIs, finer connector scopes.** CLIs that need a login
   (`gh` and `git push` first, then `vercel` and `stripe`), without the token
   ever entering a sandbox. You sign in on your computer: the daemon runs the
   CLI's own login (`gh auth login --web`, `vercel login`, `stripe login`) in
   a private temporary folder and keeps the token in the keychain, or you
   paste one in the Vault. In the sandbox the CLI holds only a stand-in token,
   and its traffic goes through the daemon, which swaps in the real one only
   for that service's API hosts and only for agents you assigned the login;
   every other host gets a plain tunnel. Risky commands keep their cards
   (merging, deploying to production, moving money; deleting a repository and
   force-pushing ask even in Auto mode). Needs a certificate the sandbox
   trusts (new crate `rcgen`). Not possible for CLIs that sign each request
   with the key (AWS, Google Cloud) or pin their certificates. Per-tool
   scopes inside a connector (for example read-only GitHub tools), finer than
   today's switches per tool group. **Done** in 0.1.11: tools per
   connector, and the GitHub login for `gh` and `git push` (token pasted in
   Vault). Open: the daemon running `gh auth login --web` itself, and Vercel
   and Stripe.
9. **Faster, more natural calls.** Reasoning switched off or a fast model used
   only in calls, a setting for the pause that ends your turn, and calls
   among agents without you, with a pop-up to join. **Done** in 0.1.11:
   a model per agent for calls, the pause setting, and calls among agents
   (`call_agents`) with a pop-up to join. Reasoning settings per call: open.
10. **Development builds.** A stable signature for development builds, so
    macOS stops asking for keychain access after every rebuild.
11. **Run in the cloud.** ferrobot on a server around the clock, reachable
    from anywhere: a sealed secret store for hosts without a keychain, the
    daemon serving the web UI, a server package, then a guest channel through
    the runner and sandbox backends for AWS Lambda MicroVMs and Firecracker on
    EC2. Plan: [docs/plans/cloud.md](../blob/main/docs/plans/cloud.md).
12. **Later.** Moving warm agents to cold sleep under memory pressure (F4d), a
    native macOS app on the same `/v1` API, and optional self-hosted
    S3-compatible storage. New ideas go on [[Ideas]] first.

Status words: **done** (built and tested, gaps noted), **partial** (code
written, key parts never run), **open** (not built), **deliberately
skipped** (left out on purpose).

## Storage

| Phase | Status | Notes |
| --- | --- | --- |
| M1 Storage layer | done | Snapshots and archives go through `object_store`: the data dir by default, S3 or R2 by URL. Never run against a real bucket. |

## Model providers and harnesses

| Phase | Status | Notes |
| --- | --- | --- |
| L0 Providers, keychain, per-agent model | done | The keychain path ran once, on macOS; Windows and Linux never. |
| L1 OpenAI-compatible driver | done | Live tool-using turns against Ollama and OpenRouter. |
| L2 Bedrock and Vertex | partial | Written; request shapes checked by capture. No call to AWS or Google Cloud yet. |
| L3 Codex driver and ChatGPT sign-in | done | A live turn ran against Ollama's Responses API. No OpenAI key used; a real ChatGPT sign-in was never tried. |
| L4 ACP driver (Gemini CLI) | done | Tested against a fake Gemini API only. |
| L5 Claude subscription | done, opt-in | No sign-in inside ferrobot: a Claude Code (your own login) provider runs your own, logged-in `claude` CLI on the host. Live on macOS; Windows never. |

## Firecracker and sleep

None of the Firecracker work has run on a KVM host: the code was written and
tested against fakes on a machine without KVM.

| Phase | Status | Notes |
| --- | --- | --- |
| F0 Firecracker spike | partial | API client, boot sequence, guest init and rootfs script written. Never booted. |
| F1 Firecracker backend | partial | Create, wake, stop, destroy, TAP networking and home disk written, marked `TODO(kvm)`. |
| F2 Guest agent over vsock | partial | `exec` and files work against a socket mock; never inside a VM. |
| F3 Jailer | open | Firecracker would run as the daemon's user. |
| F4a Sleep levels and warm sleep | done for Docker | Docker pauses idle agents. The Firecracker pause is written, untested. |
| F4b Cold sleep from a snapshot | open | Cold sleep is a plain stop on every backend. |
| F4c Archive to object storage | done for Docker | Encrypted at rest, restored on wake. The Firecracker archive depends on F4b. |
| F4d Move warm agents to cold under memory pressure | open | Optional. |
| F5 Image pipeline and KVM tests | partial | The rootfs build script exists; no KVM runner in CI. |

## Product

| Phase | Status | Notes |
| --- | --- | --- |
| Vault | done | Every key in one place; agents ask for a connector token on a card whose value they never see. |
| MCP catalog and connection check | done | Keyless servers ran live; an OAuth sign-in with a catalog server has not been tried. |
| CLIs in the sandbox image | done | Docker only; keyless CLIs only. |
| Tool scopes per agent | done | Enforced in the daemon; built-in tools switched off per driver. |
| Routines made by agents | done | With an approval card unless Auto mode is on. |
| Voice calls and call rooms | done | Used live on macOS with ElevenLabs. |
| Update button | partial | The source-build path ran in a throwaway checkout; the release updater has never run against a real release. |
| Usage chart and reported cost | done | OpenRouter calls count at the cost OpenRouter reports. |

## Release

| Phase | Status | Notes |
| --- | --- | --- |
| v0.1.0 | done | Desktop downloads for macOS (universal) and Windows (x64) that bundle the daemon; the Windows installer has had little testing. |
| v0.1.1 | done | Codex setup help, key links per provider, and the reason under a message that was not delivered. |
| v0.1.2 | done | The first-run setup shows the ChatGPT sign-in for Codex once. |
| v0.1.3 | done | Opt-in Claude Code (your own login): your own claude CLI and subscription, on the host. |
| v0.1.4 | done | Group chats, reactions, uploads, richer chat, plan limits, tags and model switch, Appearance, shortcuts, the call bar. |
| v0.1.5 | done | Fix: a message no longer shows as queued while its turn runs. |
| v0.1.6 | done | Claude Code hooks, plugins and subagents, trace export to Phoenix and Langfuse, memory in an Obsidian vault, local MCP servers, trace view, compact. |
| v0.1.7 | done | Pins for replies, files and agents; agents hear about renames, and old names of renamed agents work again. |
| v0.1.8 | done | Dashboards, webhooks and a Discord bridge, trace waits and handoffs, deleted agents named in mentions, Auto mode keeps a card for the riskiest steps. |
| v0.1.9 | done | Pins per chat, a redesigned Webhooks page, an agent's webhooks in its details, Discord reactions and a Discord chip in the chat. |
| v0.1.10 | done | Cards answered from Discord, agents posting to Discord on their own, emoji-only replies, more Appearance settings, a faster first wake and smaller updates. |
| v0.1.11 | done | Files and a console in the Computer tab, a quicker model in calls, calls among agents, Stop for a chain of work, a GitHub login for CLIs and per-tool connector scopes. |

Ideas beyond this list are on [[Ideas]].

## Help wanted

The biggest gap is Firecracker on real hardware. If you have a Linux host with
KVM, these are written up as self-contained tasks in
[docs/launch/issues/](../tree/main/docs/launch/issues):

- [Boot the Firecracker backend on KVM](../blob/main/docs/launch/issues/firecracker-on-kvm.md)
- [Start Firecracker through the jailer](../blob/main/docs/launch/issues/firecracker-jailer.md)
- [Snapshot-based cold sleep](../blob/main/docs/launch/issues/firecracker-snapshot-sleep.md)

Live runs with real provider keys help too: most [[Providers]] have only run
against fakes.
