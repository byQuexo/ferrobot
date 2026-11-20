# Conventions

Short on purpose. CI enforces what a tool can check; reviews check the rest.

## Rust

- `cargo fmt` (`rustfmt.toml`) and `cargo clippy --all-targets --all-features -- -D warnings`
  with `pedantic` on (workspace `Cargo.toml`). An `#[allow(...)]` needs a `reason = "..."`.
- `unwrap`, `expect` and `panic!` only in tests and at startup (clippy `unwrap_used`,
  `expect_used`, `panic`; `clippy.toml` allows them in tests). Request paths return errors.
- Errors: one `thiserror` enum per library crate (`CoreError`, `StoreError`, `SandboxError`);
  the daemon answers with `ApiError` (JSON `{"error", "message"}`); `anyhow` only in `main.rs`.
- Logs go through `tracing`. Never log a key, token, prompt or completion.
- Small modules, `pub` only for what another crate uses, `unsafe` is forbidden.
- `core`, `store` and `sandbox` never depend on axum or Tauri; `mcp` never on Tauri.

## TypeScript (runner, desktop)

- `strict` (plus `noUncheckedIndexedAccess`), no `any`, ESLint and Prettier
  (`.prettierrc.json`): `npm run lint` and `npm run format:check`.
- Named exports only; `async`/`await`, no `.then()` chains.
- Validate untrusted input at the boundary with zod (the runner's daemon frames
  in `protocol.ts`); new parsers of driver or tool output use zod too. The
  desktop trusts the generated `/v1` types.
- Files are kebab-case (`mcp-client.ts`); Preact component files are PascalCase,
  named after their component (`Chat.tsx`).

## Naming

- Rust `snake_case`, TS `camelCase`, types `PascalCase` in both.
- API paths are `/v1/<plural-noun>` for collections (`/v1/agents/{id}/routines`);
  uncountable resources stay singular (`/v1/memory`, `/v1/health`). A `/v1` path
  is never renamed: changes are additive, breaking ones need `/v2`.

## Errors shown to people

Short and actionable: say what failed and what to do (`Docker is not running. Start
Docker Desktop and try again.`). No stack traces or internal ids in UI text.

## Comments

- Only the why the code cannot show: a non-obvious decision, a security
  invariant, a protocol quirk, a link to a spec. One line, two at most.
- No comments that restate the code, narrate history ("now we ...", "changed X")
  or go stale. Delete a comment rather than update it once the code says it.
- Public items get a one-line doc comment; API doc comments become the public
  spec (`protocol/`), so they must match the code.
- `TODO` only with an owner or an issue: `TODO(kvm): ... (docs/launch/issues/<file>.md)`.
- Known limits get `shortcut: <the limit>, <when to upgrade>`.

## Tests

- Keep tests that guard behaviour and security invariants: secrets never reach a
  sandbox, auth and signatures, path and prefix checks, migrations, the proxy key
  swap, archive crypto, driver turn contracts, one end-to-end run.
- Prefer one table-driven test over many near-identical ones. No tests of library
  behaviour, private details or large snapshots.
- New behaviour gets a test. Never claim a result that was not run.

## Git

- Conventional Commits, one logical change per commit, imperative, at most 72 characters:
  `feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`. PR titles use the same form.
- Branches: `feat/<slug>`, `fix/<slug>`, `chore/<slug>`.
- Every gate in [CLAUDE.md "Commands"](../CLAUDE.md#commands) passes before a merge.
