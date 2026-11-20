# Daemon: build on Windows and run the keychain path

**Labels:** `help wanted`, `area: daemon`

## Context

Provider keys, the archive key, the file-link key, Codex sign-ins and
connector tokens live in the OS keychain through the `keyring` crate
(`crates/daemon/src/secrets.rs`): macOS Keychain, Windows Credential Manager or
the Secret Service. Only the macOS path has run for real (the live smoke test
in `docs/progress.md`). Windows has never run, and neither has the Linux
Secret Service.

The workspace probably does not compile on Windows yet:
`crates/sandbox/src/firecracker/client.rs` and `guest.rs` use
`tokio::net::UnixStream`, and the module is not behind a `cfg`. Nobody has
tried, so finding out is the first step.

## Tasks

- [ ] `cargo build -p ferrobot-daemon` on Windows. If it fails on the
      Firecracker module, gate it to Linux/Unix and make `--backend firecracker`
      refuse clearly elsewhere.
- [ ] Fix other Unix-only spots you hit. File modes are behind `cfg(unix)` in
      `auth.rs`, `secrets.rs` and `codex.rs`. On Windows, `restrict` in
      `auth.rs` does nothing, so the operator token file gets the folder's
      default permissions: check who can read it.
- [ ] Run the daemon with Docker Desktop on Windows. Add a provider (Settings or
      `POST /v1/providers`), check the entry in Credential Manager, run one
      turn (Ollama or a real key), delete the provider and check the entry is
      gone.
- [ ] `scripts/smoke-live.sh` is bash and checks the keychain with macOS
      `security`. Either make its keychain check work on Windows (Git Bash or
      WSL against the Windows daemon) or write the manual steps down.
- [ ] Record the result in `docs/progress.md`. A report from Linux (Secret
      Service) is welcome in the same issue.

## Acceptance criteria

- `cargo build --workspace` and `cargo test --workspace` pass on Windows, or
  the failing tests are listed with the reason.
- A key stored through the API lands in Credential Manager, the model proxy
  uses it, it never shows up in the sandbox, and deleting the provider removes
  it.
- `docs/progress.md` says what ran, on which Windows version.

## Pointers

- `crates/daemon/src/secrets.rs` (`SecretStore`, `check_keyring`).
- `crates/sandbox/src/lib.rs` (`mod firecracker`),
  `crates/sandbox/src/firecracker/client.rs`, `guest.rs`.
- `crates/daemon/src/main.rs` (`Backend`, the `cfg(not(unix))` branch).
- `scripts/smoke-live.sh` (`in_keychain`).
- `docs/progress.md` ("Live smoke test (hardening pass)"), `docs/decisions.md`
  ("Phase 2: Providers and drivers").
