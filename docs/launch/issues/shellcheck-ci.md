# CI: run shellcheck on the shell scripts

**Labels:** `good first issue`, `area: ci`

## Context

The shell scripts were checked only with `bash -n`, `sh -n` and `dash -n`;
shellcheck was never run on them (`docs/progress.md`, "Phase 5"). Some of them
handle keys and tokens (`scripts/smoke-live.sh`) or run as PID 1 in a VM
(`scripts/firecracker/ferrobot-init`), so quoting mistakes matter.

Scripts today:

- `scripts/*.sh`
- `scripts/firecracker/build-rootfs.sh`
- `scripts/firecracker/ferrobot-init` (POSIX sh, no extension)
- `runner/bin/ferrobot-pdf` (POSIX sh, no extension)

## Tasks

- [ ] Run shellcheck on every script and fix the findings. Where a finding is
      intended, disable it on that line with a reason:
      `# shellcheck disable=SC2086 # word splitting wanted: ...`.
- [ ] Add a job to `.github/workflows/ci.yml` with a short `timeout-minutes`,
      following the `needs` / `if` pattern of the Rust and TypeScript jobs.
      ShellCheck is listed as preinstalled on `ubuntu-latest`; confirm in the
      runner image's README, else install it with apt.
- [ ] Pass the files without an extension explicitly.
- [ ] Add the new job's name to the required checks in
      `docs/launch/github-settings.md` and `scripts/apply-github-settings.sh`.
- [ ] Keep `ferrobot-init` POSIX sh (it runs under dash) and never let it print
      the env.

## Acceptance criteria

- The new CI job passes on all scripts, and fails when a script has an unquoted
  variable (try it once on a throwaway branch).
- No behaviour change: `./scripts/smoke-docker.sh` still passes locally.

## Pointers

- `.github/workflows/ci.yml`
- `scripts/`, `runner/bin/ferrobot-pdf`
- <https://www.shellcheck.net/wiki/>
