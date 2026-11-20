# Codex: one sign-in run per provider at a time

**Labels:** `good first issue`, `area: daemon`

## Context

For `codex` providers signed in with ChatGPT, the daemon runs the host's
`codex app-server` in a fresh private `CODEX_HOME` holding the stored
`auth.json`, asks it for a token (`getAuthStatus` with `refreshToken: true`)
and stores back what Codex wrote. Runs are not locked per provider: when the
cached token expires and two proxy requests arrive at once, both start a run
from the same `auth.json` and both may use the same refresh token.
`Home::save` keeps a stale refresh from overwriting a newer sign-in, but the
double refresh itself can fail (`docs/decisions.md`, hardening item 4: "Not
done: locking runs per provider").

## Tasks

- [ ] Add a per-provider async lock. In `chatgpt_token`, take it after a cache
      miss, check the cache again, and only then call `auth_status`, so the
      second caller gets the first caller's fresh token.
- [ ] Do not hold the shared `state.codex` mutex across an app-server run: that
      would serialise every provider.
- [ ] Consider taking the same lock in `start_login` and `auth_status` calls
      from `login_status`.
- [ ] Test with the fake host `codex` used by
      `codex_providers_use_a_key_or_the_hosts_chatgpt_login`: two token
      requests at once after expiry start a single `getAuthStatus` run (for
      example, the fake script appends to a counter file).
- [ ] Update `docs/decisions.md` item 4.

## Acceptance criteria

- Concurrent token requests for one provider start one app-server run.
- Different providers still run in parallel.
- `cargo test -p ferrobot-daemon` passes.

## Pointers

- `crates/daemon/src/codex.rs`: `CodexState`, `Home::open`, `Home::save`,
  `auth_status`, `chatgpt_token`, `start_login`, `login_status`.
- `crates/daemon/src/state.rs` (`codex` field of the app state).
- `crates/daemon/src/proxy.rs` (the caller of `chatgpt_token`).
- `crates/daemon/tests/features.rs`.
