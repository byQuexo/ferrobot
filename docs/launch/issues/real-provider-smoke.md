# Providers: smoke-test OpenAI, Gemini, Bedrock and Vertex with real accounts

**Labels:** `help wanted`, `area: providers`

## Context

Every provider kind's credential handling is tested against fake upstreams.
With a real account, only two have run: Anthropic (Claude driver, key from the
macOS Keychain) and local Ollama, both through `scripts/smoke-live.sh`
(`docs/progress.md`). Never run against the real service:

- `openai`, `openrouter`
- `gemini` (its OpenAI-compatible endpoint; the flattened tool schemas are
  untested there)
- `bedrock` (Bedrock API key) and `vertex` (the daemon host's `gcloud` login)
- `codex` with an OpenAI key or a ChatGPT sign-in (it ran against Ollama's
  Responses API only)
- `gemini_cli` (ran against a fake Gemini API only)

Real calls cost money: use the smallest model that can call a tool.

## Tasks

- [ ] Extend `scripts/smoke-live.sh` so it can run an agent on another provider
      kind and model. Read keys hidden, like the Anthropic key; never print
      one. Vertex needs project and region, Bedrock a region.
- [ ] Keep the leak check: the real key must not appear in the container env,
      its files, the container logs, the daemon log or the data dir.
- [ ] Run one tool-using turn per provider you have access to. One provider per
      pull request is fine.
- [ ] Add the results to `docs/progress.md` as a table (provider, model, date,
      result, what was not checked).
- [ ] File a bug for each failure, for example a tool schema the upstream
      rejects, or a proxy `404` for an endpoint the harness calls.

## Acceptance criteria

- For each provider run: the turn completes with a tool call, the nonce check
  passes and the leak check finds nothing.
- `docs/progress.md` lists what ran and what did not.

## Pointers

- `scripts/smoke-live.sh`
- `crates/core/src/provider.rs` (kinds, drivers, default upstreams).
- `crates/daemon/src/proxy.rs` (allowed calls per API family).
- `crates/daemon/src/api/providers.rs`
- `docs/decisions.md` ("Phase 2", "Model proxy hardening"),
  `docs/progress.md` ("Phase 2", "Phase 3", "L4", "Live smoke test").
