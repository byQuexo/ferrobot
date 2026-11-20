# Firecracker: cold sleep with snapshot and load

**Labels:** `help wanted`, `area: firecracker`

## Context

Warm sleep pauses the VM (`PATCH /vm` to `Paused`). Cold sleep is a plain stop
today: the guest shuts down and a wake boots it fresh, so the runner restarts
(the home disk is kept). Roadmap F4b: cold sleep should pause the VM, write a
full snapshot (`PUT /snapshot/create`) and stop the process; a wake loads it
(`PUT /snapshot/load`) and resumes where it left off.

The API client already builds both requests (`Request::snapshot_create`,
`Request::snapshot_load`, tested against the fake API), but nothing calls them
yet. `snapshot` returns `NotImplemented`. Both spots are marked `TODO(kvm)`.

Depends on [firecracker-on-kvm.md](firecracker-on-kvm.md).

## Tasks

- [ ] `sleep(Cold)`: pause, `snapshot_create` (state and memory file in the VM
      directory), stop the process.
- [ ] Wake from a snapshot: spawn `firecracker`, `snapshot_load` with
      `resume_vm`, then delete the snapshot files.
- [ ] The memory file holds the agent token (it is in guest memory): create it
      with mode 0600 and never leave it behind after a load.
- [ ] Check on KVM that the TAP device, vsock socket and disks match what the
      snapshot expects, and that the home disk is not changed between snapshot
      and load.
- [ ] Check that the runner's WebSocket to the daemon recovers after a long
      sleep.
- [ ] Decide whether operator snapshots (`snapshot`) reuse this or stay
      `NotImplemented`; write it down in `docs/decisions.md`.
- [ ] Unit tests: the call sequence against the fake API, like the boot
      sequence tests.
- [ ] Drop the `#[allow(dead_code, ...)]` on `Request::snapshot_create` and
      `Request::snapshot_load` once they are called.
- [ ] Update `docs/progress.md` and `scripts/firecracker/README.md`.

Out of scope: archiving a Firecracker sandbox to object storage (F4c).

## Acceptance criteria

- On a KVM host: write a file to `/tmp` in the guest (a tmpfs, so a reboot
  clears it), cold-sleep the agent, send it a message. The file is still there
  and the runner did not restart.
- No memory file is left on disk after a wake, and none is ever world-readable.
- The `TODO(kvm)` markers in `snapshot` and `sleep` are gone.

## Pointers

- `crates/sandbox/src/firecracker/mod.rs`: `sleep`, `snapshot`, `wake`, `stop`.
- `crates/sandbox/src/firecracker/client.rs`: `Request::snapshot_create`,
  `Request::snapshot_load`, `VmState`.
- `crates/sandbox/src/lib.rs`: `SleepLevel`, `supports_snapshots`.
- `docs/decisions.md` ("Phase 4", "Phase 5").
- Upstream: <https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md>
