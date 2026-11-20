# Sandbox: archive only the container's changes, not the whole image

**Labels:** `help wanted`, `area: sandbox`

## Context

Archiving a sleeping Docker sandbox (`--archive-after`, or
`POST /v1/agents/{id}/stop?level=archived`) commits the container and streams
`docker image save` of the result into the object store, encrypted. That
includes the runner image's base layers, which are the same for every agent:
the live run in `docs/progress.md` ("F4c") wrote a 1.29 GB `image.tar` next to
a 188 KB `home.tar`. See the `shortcut:` comment in
`DockerBackend::archive_sandbox`.

## Tasks

- [ ] Pick an approach and write it down in `docs/decisions.md` ("F4c:
      archive"). For example: keep only the layers the commit added on top of
      the runner image, or archive the container's changed paths
      (`docker diff`) and replay them onto a fresh container from the base
      image. Deleted files must stay deleted.
- [ ] Store the base image id with the archive. Restoring onto a different
      runner image must either work or fail with a clear message.
- [ ] Keep what the current path guarantees: encryption (`sealer`), streaming
      without holding the archive in memory (`put_stream`), and the blanked
      token in committed images (`BLANK_TOKEN`).
- [ ] Old full-image archives must still restore.
- [ ] Extend `archive_moves_the_sandbox_into_the_store_and_back` in
      `crates/sandbox/tests/docker.rs`: a change outside the home directory
      survives, and the archive is far smaller than the image.
- [ ] Remove the `shortcut:` comment; update `docs/progress.md`.

## Acceptance criteria

- Archiving a fresh agent writes megabytes, not a gigabyte.
- Restore brings back the home directory and root filesystem changes.
- `cargo test -p ferrobot-sandbox --features docker-tests --test docker` passes.

## Pointers

- `crates/sandbox/src/docker.rs`: `archive_sandbox`, `unarchive`, `archive_keys`,
  `put_stream`, `sealer`, `BLANK_TOKEN`.
- `crates/sandbox/src/seal.rs` (archive encryption).
- `crates/sandbox/tests/docker.rs`.
- `docs/decisions.md` ("F4c: archive", "Hardening after the external review",
  item 3).
