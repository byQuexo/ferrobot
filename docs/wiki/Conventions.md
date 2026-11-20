The source of truth is
[docs/CONVENTIONS.md](../blob/main/docs/CONVENTIONS.md). This page is a short
summary; when the two differ, that file wins.

## Rust

- `cargo fmt` and `cargo clippy` with `pedantic` on and `-D warnings`. An
  `#[allow(...)]` needs a `reason = "..."`. `unsafe` is forbidden.
- No `unwrap`, `expect` or `panic!` outside tests and startup: request paths
  return errors.
- Errors: one `thiserror` enum per library crate, `ApiError` in the daemon
  (JSON `{"error", "message"}`), `anyhow` only in `main.rs`.
- Log through `tracing`. Never log a key, token, prompt or completion.
- The core crates (`core`, `store`, `sandbox`, `mcp`) never depend on axum or
  Tauri.

## TypeScript

- `strict`, no `any`, ESLint and Prettier.
- Named exports, `async`/`await`.
- Validate untrusted input at the boundary (zod in the runner).
- Files are kebab-case; Preact component files are PascalCase.

## API

- Paths are `/v1/<plural-noun>`; uncountable resources stay singular
  (`/v1/memory`).
- Changes are additive. A `/v1` path is never renamed; breaking changes need
  `/v2`.
- `protocol/` is generated from the code, never edited by hand.

## Comments and tests

- Comments say only the why the code cannot show, in one or two lines.
- `TODO` needs an owner or an issue, e.g. `TODO(kvm): ...`. Known limits get
  `shortcut: <the limit>, <when to upgrade>`.
- New behaviour gets a test. Keep the tests that guard security invariants
  (secrets never reach a sandbox, auth, the proxy key swap, archive crypto).
- Never claim a result that was not run.

## Git

- Conventional Commits (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`,
  `chore:`), imperative, at most 72 characters, one logical change per
  commit. PR titles use the same form.
- Branches: `feat/<slug>`, `fix/<slug>`, `chore/<slug>`.
- All CI gates pass before a merge.

## Error messages

Short and actionable: say what failed and what to do, e.g. "Docker is not
running. Start Docker Desktop and try again." No stack traces or internal ids
in UI text.
