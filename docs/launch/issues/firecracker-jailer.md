# Firecracker: start VMs through the jailer

**Labels:** `help wanted`, `area: firecracker`

## Context

`FirecrackerBackend::boot` starts `firecracker --api-sock ...` directly, so
every VM runs as the daemon's user with the daemon's view of the file system.
Firecracker's jailer (roadmap F3) runs each VM in its own chroot, namespaces
and cgroup as an unprivileged uid/gid. The spawn is marked
`TODO(kvm): start through the jailer`.

Depends on [firecracker-on-kvm.md](firecracker-on-kvm.md): a VM has to boot
first.

## Tasks

- [ ] Add settings for the jailer binary and the uid/gid to run VMs as
      (CLI flag plus `FERROBOT_FC_*` env, like the other `--fc-*` flags).
- [ ] Spawn the jailer instead of `firecracker` (`--id`, `--exec-file`,
      `--uid`, `--gid`, `--chroot-base-dir`; see upstream `docs/jailer.md`).
- [ ] Put the kernel, root filesystem, home disk, API socket and vsock socket
      inside each VM's chroot (hard links or bind mounts), and make the paths
      in the boot sequence chroot-relative.
- [ ] Keep the runner env out of argv and disk: MMDS only, as today.
- [ ] `destroy` cleans up the chroot; the home disk stays unless purged.
- [ ] Unit tests for the pure parts (jailer argv, path mapping), next to the
      existing boot-sequence tests.
- [ ] Update `scripts/firecracker/README.md` ("Not done yet") and
      `docs/progress.md`.

## Acceptance criteria

- Each VM's `firecracker` process runs as the configured unprivileged uid, in
  its own chroot and mount namespace (check `/proc/<pid>/status` and
  `/proc/<pid>/root`).
- Everything in the acceptance criteria of
  [firecracker-on-kvm.md](firecracker-on-kvm.md) still passes.
- The jailer `TODO(kvm)` in `boot` is gone.

## Pointers

- `crates/sandbox/src/firecracker/mod.rs`: `FirecrackerConfig`,
  `FirecrackerConfig::boot_sequence`, `FirecrackerBackend::boot`, `destroy`.
- `crates/daemon/src/main.rs`: the `--fc-*` flags.
- `scripts/firecracker/README.md`.
- Upstream: <https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md>
