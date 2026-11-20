# Contributing

Thanks for helping. This page covers setup, the checks to run and how a change
gets merged. The rules for the code itself are in
[docs/CONVENTIONS.md](docs/CONVENTIONS.md).

## Prerequisites

- Rust, stable channel (`rust-toolchain.toml` pins it; MSRV 1.88).
- Node.js 24.
- Docker Desktop or Podman: the default sandbox backend and the end-to-end tests.
- Optional: [Ollama](https://ollama.com), to run real tool-using turns without
  an API key.
- Only for the Tauri shell on Linux: `libwebkit2gtk-4.1-dev` and `libsoup-3.0-dev`.
- Only for Firecracker work: Linux with KVM (see
  [scripts/firecracker/README.md](scripts/firecracker/README.md)).

## Setup

```bash
git clone git@github.com:<you>/<your-fork>.git
cd <your-fork>
cargo build --workspace
npm --prefix runner ci && npm --prefix runner run build
(cd apps/desktop && npm ci)
docker build -f runner/Dockerfile -t ferrobot-runner:dev .   # runner image
```

The [README](README.md#quick-start) shows how to start the daemon and talk to
an agent. The mock driver (`--driver mock`) needs no API key.

## Checks

Run the checks for the parts you touched. All of them must pass before a merge.

Rust (repo root):

```bash
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo check -p ferrobot-core -p ferrobot-store -p ferrobot-sandbox
```

API spec drift (`protocol/` is generated from the code, never edited by hand):

```bash
cargo test -p ferrobot-daemon --test specs                          # check
FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs  # regenerate
cd apps/desktop && npm run gen:api     # regenerate TS types; check:api = gen + git diff
```

Runner (`runner/`):

```bash
npm ci && npm run lint && npm run format:check && npm run typecheck && npm test && npm run build
```

Desktop (`apps/desktop/`):

```bash
npm ci && npm run lint && npm run format:check && npm run check:api && npm run typecheck && npm test && npm run build
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings
```

The built `dist/` is embedded at compile time: run `npm run build` before
`cargo build` in `src-tauri`.

End to end:

```bash
cargo test -p ferrobot-daemon --test e2e -- --ignored                # local backend (needs runner/dist)
docker build -f runner/Dockerfile -t ferrobot-runner:dev .           # runner image
cargo test -p ferrobot-sandbox --features docker-tests --test docker
cargo test -p ferrobot-daemon --features docker-tests --test e2e -- --ignored
./scripts/smoke-docker.sh            # real daemon + Docker + mock driver
sudo scripts/firecracker/build-rootfs.sh   # Linux only; see scripts/firecracker/README.md
```

Demo data, if you want a filled UI:

```bash
FERROBOT_DATA_DIR=/tmp/fbdemo ./target/debug/ferrobotd --secret-store file --backend docker --driver mock --idle-timeout 0
FERROBOT_DATA_DIR=/tmp/fbdemo bash scripts/seed-demo.sh [avatar.png]
```

Clean up afterwards: delete the demo agents (`DELETE /v1/agents/{id}`), stop
the daemon, and check that `docker ps -a` shows no containers left.

CI runs the Rust and TypeScript checks on pull requests. The Tauri shell
is checked locally only, and the Docker end-to-end job runs only when a
maintainer starts it, so run those yourself when you touch them. Say in the
pull request what you did not run.

## Pull requests

1. Fork the repository and clone your fork.
2. Branch from `main`: `feat/<slug>`, `fix/<slug>` or `chore/<slug>`.
3. Make the change. New behaviour gets a test.
4. Run the checks above until they pass.
5. Open a pull request against `main` and fill in the template.

A maintainer reviews it. CI for a pull request from a fork may wait until a
maintainer approves the run.

## Commits

[Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`,
`refactor:`, `test:`, `docs:`, `chore:`. Imperative, one logical change per
commit, at most 72 characters. Pull request titles use the same form.

```
docs: explain --warm-after in the quick start
```

## Rules in short

- [docs/CONVENTIONS.md](docs/CONVENTIONS.md) is the rule book.
- Clean room: no code, text or assets from other agent platforms under
  non-open licenses.
- Provider keys and the operator token never go into a sandbox, its env, a log, a
  test fixture or an issue.
- Contributions are dual-licensed under Apache-2.0 and MIT, as described in the
  [README](README.md#license).

## Where to start

Ready-to-file tasks are in [docs/launch/issues/](docs/launch/issues). On
GitHub, look for the `good first issue` label for small, well-scoped tasks
and `help wanted` for bigger ones, such as Firecracker on a KVM host.

## Security and conduct

Report vulnerabilities privately, as described in [SECURITY.md](SECURITY.md),
never in a public issue. Everyone here follows the
[Code of Conduct](CODE_OF_CONDUCT.md).
