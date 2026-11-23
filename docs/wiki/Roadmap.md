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
   out unless you turn them on.
6. **Later.** Moving warm agents to cold sleep under memory pressure (F4d), a
   native macOS app on the same `/v1` API, and optional self-hosted
   S3-compatible storage. Smaller ideas are on [[Ideas]].

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
