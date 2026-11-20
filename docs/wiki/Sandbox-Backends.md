Every agent runs in its own sandbox. The daemon reaches sandboxes only
through one trait (`SandboxBackend`), and `--backend` picks the
implementation.

| `--backend` | What it is | Status |
| --- | --- | --- |
| `docker` (default) | One container per agent, through a Docker (or Podman) socket. | Tested end to end, in CI and by hand. |
| `local` | The runner as a plain host process. **No isolation.** | Development and CI only. |
| `firecracker` | One Firecracker microVM per agent. Linux with KVM only. | Written, **never booted**. |

There is also a Sprites backend in the code. It is a stub: every operation
returns "not implemented", and `--backend` does not offer it.

## Docker

- Each agent gets a container from the runner image (`--runner-image`) with
  its home directory (`/home/node`) on a named volume.
- Containers run as a non-root user with all capabilities dropped,
  `no-new-privileges`, an init process and limits of 2 GiB memory and 512
  processes.
- Containers reach the daemon's agent API at `host.docker.internal`, so start
  the daemon with `--agent-listen 0.0.0.0:7879`. Every request there needs
  the agent's own token.
- Crashed containers are restarted by the supervisor.
- Snapshots (root filesystem image plus a home archive) can be taken,
  restored and deleted.

## Local

`--backend local` runs the runner as a child process of the daemon, in a
directory under the data dir. Agents can read and change anything your user
can. Use it for development and CI without Docker only. It can take snapshots
(directory copies) but not restore them, and it cannot pause.

## Sleep levels

Idle agents go to sleep in steps. An agent never sleeps while it works or has
queued mail. A message, a routine or an operator command (`wake`, `exec`)
wakes it again.

| Level | What happens | When | Backends |
| --- | --- | --- | --- |
| Warm | Paused: processes frozen, memory kept, quick wake. | `--warm-after` (default 300 s, 0 = never) | Docker (`docker pause`). Backends that cannot pause stay awake until the cold timeout. |
| Cold | Stopped: memory freed, disk kept. | `--idle-timeout` (default 900 s, 0 = never) | All |
| Archived | Moved to object storage; nothing is left on the host. Restored on wake. | `--archive-after` (default 0 = off) | Docker only |

The operator can also put an agent to sleep by hand:
`POST /v1/agents/{id}/stop?level=warm|cold|archived` (default `cold`). On a
backend that cannot pause, `level=warm` answers `501`. Agent status shows
`paused`, `stopped` or `archived`.

A long warm pause with a real runner and open model connections has not been
measured.

## Encrypted archives

An archive holds the container image and the home directory, streamed into
the object store under `--storage-url` (default: the data dir; `s3://` for S3
or R2).

- Archives and snapshot home archives are encrypted with AES-256-GCM in
  64 KiB chunks. Each object is bound to its path, so a dropped, reordered or
  moved chunk fails to open.
- The key (`archive-key`) is made on first start, kept in the OS keychain and
  never replaced. There is no export or escrow: **losing the key loses the
  archives.**
- With a remote store (`s3://`), the daemon refuses to start without the key.
  With the local data dir it warns and writes plaintext if the key is
  unusable.
- The agent token inside an archived sandbox is retired when it is archived;
  a restored sandbox gets a new one.
- Not encrypted: files agents share, and snapshot images kept in the local
  Docker engine.
- Each archive stores the whole container image, so archives are large (more
  than a gigabyte each).
- No real S3 or R2 bucket has been used yet.

## Firecracker

**No microVM has ever booted.** The code was written and tested on a machine
without KVM, against fakes. Every untested spot is marked `TODO(kvm)` in the
code.

What exists:

- A thin client for Firecracker's API socket and the boot sequence. The
  runner's environment goes through MMDS, never kernel arguments or disk.
- Networking: one TAP device per VM on a shared bridge (`fcbr0`, host
  `172.16.0.1`); guests reach the daemon's agent port only, given the
  firewall rules in the setup guide.
- A home disk per VM, formatted by the guest init on first boot.
- `exec` and file access through a small guest agent over vsock.
- Warm sleep through Firecracker's pause. Cold sleep is a plain stop for now.
- A script that builds the guest root filesystem from the runner image.

What is missing: a first boot on a KVM host, the jailer (Firecracker
currently runs as the daemon's user), snapshot-based cold sleep, snapshots,
archives, DNS in the guest, and a clean guest shutdown when the daemon stops.

Host requirements and setup:
[scripts/firecracker/README.md](../blob/main/scripts/firecracker/README.md).
If you have a Linux host with KVM, this is the most useful place to help; see
[[Roadmap]].
