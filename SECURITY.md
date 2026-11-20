# Security policy

## Reporting a vulnerability

Report it privately through GitHub Security Advisories: open the repository's
**Security** tab and click **Report a vulnerability**. Do not open a public
issue, discussion or pull request for it.

Please include what you found, how to reproduce it, the version or commit, the
sandbox backend and provider, and what an attacker gains. Remove real keys and
tokens from logs before you attach them. We answer in the advisory and agree a
disclosure date with you; please give us time to ship a fix first.

## Supported versions

| Version | Supported |
| --- | --- |
| 0.1.x | yes |
| older | no |

## In scope

- **Daemon API auth:** the operator token on `/v1`, the loopback-only default,
  and the routes that need no token (`/v1/health`, the OAuth callback).
- **Model proxy key handling:** provider keys, `ANTHROPIC_API_KEY` and ChatGPT
  tokens must stay in the daemon. Any way to read a key from a sandbox, a log,
  an event or an API response, or to use it for calls outside the proxy's
  allowlist, is in scope.
- **Sandbox escape paths:** from an agent's container (or microVM) to the host,
  to the daemon beyond the agent API, or to another agent's sandbox.
- **Agent token handling:** per-agent tokens on `/runner/ws`, `/mcp` and
  `/model`. An agent acting as another agent, or an old token still working, is
  in scope.
- **OAuth connectors:** sign-in, token storage, refresh, revocation, issuer
  checks, and connector tokens reaching a sandbox.
- **Signed file links:** `/artifacts/...` links (HMAC, 30-minute expiry), the
  page sandbox and CSP, and reading another agent's files.

Out of scope: what someone holding the operator token can do (it grants full
control, including `exec` in every sandbox, by design); the `local` backend
(development only, no isolation); `--secret-store file` (plaintext, meant for
headless CI); bugs in dependencies with no path through this code (report
those upstream).

## Known open hardening items

These are known and documented in [docs/decisions.md](docs/decisions.md) and
[docs/progress.md](docs/progress.md). Reports that go beyond them are welcome.

- **Firecracker has never booted a microVM,** and there is no jailer yet:
  `firecracker` would run as the daemon's user. Guests on the bridge are
  isolated from each other, not from the host, unless you add the firewall
  rules in [scripts/firecracker/README.md](scripts/firecracker/README.md).
- **Shared files (`artifacts/`) are not encrypted at rest.** With S3 storage
  they sit there in the clear. Snapshot images in the local Docker engine are
  not encrypted either. Archives and snapshot home archives are, except that
  with a local store and a broken archive key the daemon warns and writes them
  unencrypted.
- **Old tokens in the home directory:** a token is rotated on archive and
  blanked in committed images, but files in an agent's home (for example tool
  configs) may still hold the old, dead token.
- **Global memory:** an agent can only change its own global memory entries,
  but a hijacked agent can still add new entries that every agent reads.
- **Agent pages:** an HTML page an agent shares runs sandboxed and may not load
  from or connect to any remote origin, but it can still show misleading
  content (phishing-like UI).
- **No per-link revocation** of shared-file links: rotating
  `artifact-link-key` in the keychain kills every link at once.
- **Bedrock supports API keys only,** no IAM credentials or SigV4 signing.
- **The real OS keychain path ran on macOS only.** Windows Credential Manager
  and the Linux Secret Service have never run, and the check that deleting a
  provider removes its keychain entry was fixed but not re-run.
- **Two Codex token refreshes can race:** runs are not locked per provider, so
  two runs refreshing at once could each use the same refresh token.
- **Losing the archive key loses the archives:** it lives only in the keychain
  (or `secrets.json`), with no export or escrow.
- **Runner guard rails mostly do not apply** to the Codex and Gemini CLI
  harnesses, which run many commands without asking; the sandbox is the
  boundary there.
- **The operator API is plain HTTP** on `127.0.0.1` and is meant for local use
  only.
- **Containers share the host kernel.** A kernel exploit inside a Docker
  sandbox reaches the host; microVMs (Firecracker, not ready) would close that.
- **Unmaintained dependencies** without a maintained successor (`chrono`,
  `humantime`, and a few in the Tauri stack) are listed in
  [docs/decisions.md](docs/decisions.md); CI fails on known vulnerabilities.
