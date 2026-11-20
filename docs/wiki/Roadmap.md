Status per phase. The detailed log of what was built and tested is
[docs/progress.md](../blob/main/docs/progress.md); the assumptions behind it
are in [docs/decisions.md](../blob/main/docs/decisions.md).

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
| L1 OpenAI-compatible driver | done | Live tool-using turns against Ollama only. |
| L2 Bedrock and Vertex | partial | Written; request shapes checked by capture. No call to AWS or Google Cloud yet. |
| L3 Codex driver and ChatGPT sign-in | done | A live turn ran against Ollama's Responses API. No OpenAI key used; a real ChatGPT sign-in was never tried. |
| L4 ACP driver (Gemini CLI) | done | Tested against a fake Gemini API only. |
| L5 Claude subscription sign-in | deliberately skipped | By design: no Claude subscription logins. Claude runs on API keys, Bedrock or Vertex. |

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

## Release

| Phase | Status | Notes |
| --- | --- | --- |
| v0.1.0 | in progress | The [[Usage tab|Usage-and-Costs]], desktop downloads for macOS and Windows that bundle the daemon, voice calls, delegated tasks, the model picker. |

## Later

Not scheduled: a file browser and terminal in the agent's info panel, computer
use inside sandboxes (a virtual desktop, browser automation, screenshots in
the timeline), and a native macOS app on the same `/v1` API.

## Help wanted

The biggest gap is Firecracker on real hardware. If you have a Linux host with
KVM, these are written up as self-contained tasks in
[docs/launch/issues/](../tree/main/docs/launch/issues):

- [Boot the Firecracker backend on KVM](../blob/main/docs/launch/issues/firecracker-on-kvm.md)
- [Start Firecracker through the jailer](../blob/main/docs/launch/issues/firecracker-jailer.md)
- [Snapshot-based cold sleep](../blob/main/docs/launch/issues/firecracker-snapshot-sleep.md)

Live runs with real provider keys help too: most [[Providers]] have only run
against fakes.
