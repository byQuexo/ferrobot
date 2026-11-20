# Firecracker: boot a microVM on a KVM host

**Labels:** `help wanted`, `area: firecracker`

## Context

The Firecracker backend (`--backend firecracker`) is written but has never
booted a VM: it was built on macOS, which has no `/dev/kvm`. The API client,
the boot sequence and the vsock guest agent are tested only against fakes
(Unix-socket mocks of Firecracker's API and vsock). Every untested spot is
marked `TODO(kvm)` and points to this file.

You need a Linux host (x86_64 or aarch64) with read/write access to
`/dev/kvm`, root or `CAP_NET_ADMIN`, the `firecracker` binary, and Docker to
build the root filesystem.

## Tasks

- [ ] Set up the host as in `scripts/firecracker/README.md`: bridge `fcbr0`
      with `172.16.0.1/24`, the firewall rules, an uncompressed guest kernel
      with `CONFIG_IP_PNP`.
- [ ] Root filesystem: `sudo scripts/firecracker/build-rootfs.sh rootfs.ext4 2G`.
      Fix what breaks.
- [ ] Start `ferrobotd --backend firecracker --driver mock --fc-kernel vmlinux
      --fc-rootfs rootfs.ext4 --agent-listen 172.16.0.1:7879` and create an
      agent.
- [ ] **Process spawn:** `FirecrackerBackend::boot` starts `firecracker
      --api-sock`, waits for the socket and sends the boot sequence.
- [ ] **TAP and bridge:** `ensure_tap` (tuntap, bridge master, `isolated on`,
      up) and the TAP removal in `destroy`.
- [ ] **Guest network:** the kernel's `ip=<guest>::<host>:<mask>::eth0:off`
      argument from `FirecrackerConfig::boot_sequence` brings up `eth0`.
- [ ] **Init:** `scripts/firecracker/ferrobot-init` as PID 1: mounts, home disk
      formatted on first boot only (`blkid -p` exit 2), mounted at `/home/node`.
- [ ] **MMDS env:** the init's route to `169.254.169.254`, the MMDS v2 token,
      the env read key by key. The agent token must reach the runner and never
      appear on the console or in `firecracker.log`.
- [ ] **vsock guest agent:** `exec`, `read_file` and `write_file` through
      `<vm dir>/vsock.sock` (`CONNECT 52`); socat in the guest runs
      `dist/guest-agent.js` as `node`.
- [ ] **Runner reaches the daemon:** `FERROBOT_DAEMON_URL=http://172.16.0.1:7879`,
      the runner connects to `/runner/ws`, a mock turn completes.
- [ ] **Stop and sleep:** `stop` (Ctrl-Alt-Del on x86_64 with a 10 s timeout,
      hard kill on aarch64), warm sleep (pause/resume), wake after a stop.
- [ ] Remove each `TODO(kvm)` that is now tested; narrow the rest.
- [ ] Record the run in `docs/progress.md` (Phase 5): CPU architecture, host
      kernel, Firecracker version, what passed.

Report back on the open points from `docs/decisions.md` ("Phase 5"): stopping
the daemon kills all VMs (`kill_on_drop`); aarch64 has no `SendCtrlAltDel`, so
`stop` there is a hard kill; `destroy(false)` leaves the VM `Stopped`, not
`Missing`; the guest has no DNS (`/etc/resolv.conf` is empty).

## Acceptance criteria

- On a KVM host, two agents on the mock driver boot, and a message `@bob hello`
  to the first reaches the second through `send_message`.
- `POST /v1/agents/{id}/exec` runs in the guest, and the backend's `read_file`
  and `write_file` work (an agent's `share_file` goes through `read_file`).
- A file in `/home/node` survives warm sleep, a cold stop and a wake.
- The agent token is in neither `firecracker.log`, the console output nor the
  root filesystem image.
- No `TODO(kvm)` that points here is left for something that ran.

## `TODO(kvm)` sites

| Where | Issue |
| --- | --- |
| `mod.rs`: `ensure_tap`, `FirecrackerBackend::boot`, `exec` (guest agent), `stop`, TAP removal in `destroy` | this one |
| `mod.rs`: the `firecracker` spawn in `boot` | [firecracker-jailer.md](firecracker-jailer.md) |
| `mod.rs`: `snapshot`, cold `sleep` | [firecracker-snapshot-sleep.md](firecracker-snapshot-sleep.md) |
| `scripts/firecracker/build-rootfs.sh`, `scripts/firecracker/ferrobot-init` | this one |

## Pointers

- `crates/sandbox/src/firecracker/mod.rs`: `ensure_tap`,
  `FirecrackerBackend::boot`, `exec` / `read_file` / `write_file`, `stop`,
  `destroy`.
- `crates/sandbox/src/firecracker/client.rs` (API client),
  `crates/sandbox/src/firecracker/guest.rs` (vsock client),
  `runner/src/guest-agent.ts` (guest side).
- `scripts/firecracker/build-rootfs.sh`, `scripts/firecracker/ferrobot-init`,
  `scripts/firecracker/README.md`.
- `crates/daemon/src/main.rs`: `--backend firecracker` and the `--fc-*` flags.
- `docs/progress.md` ("Phase 5", "F2"), `docs/decisions.md` ("Phase 5", "F2").
- Follow-ups: [firecracker-jailer.md](firecracker-jailer.md),
  [firecracker-snapshot-sleep.md](firecracker-snapshot-sleep.md).
