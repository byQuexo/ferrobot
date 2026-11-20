# Providers: Bedrock with IAM credentials (SigV4)

**Labels:** `help wanted`, `area: providers`

## Context

Bedrock providers take a Bedrock API key only. The sandbox gets its agent token
as `AWS_BEARER_TOKEN_BEDROCK`, and the model proxy swaps in the real key as
`Authorization: Bearer`. Many AWS setups use IAM credentials instead (access
keys, a profile, SSO or an instance role), which need SigV4-signed requests.
IAM and SigV4 are not supported today (`docs/decisions.md`, "Bedrock without
SigV4").

## Tasks

- [ ] The daemon signs; AWS credentials never enter a sandbox. The sandbox keeps
      sending its agent token, the proxy checks it, removes it and signs the
      outgoing request with SigV4 (service `bedrock`, the provider's region).
- [ ] Read credentials on the daemon host from the standard `AWS_*` variables,
      as the S3 storage does; profiles, SSO and instance roles if you can.
- [ ] Decide how a provider picks API key or IAM (for example: no stored key
      means IAM) and keep the API-key path unchanged.
- [ ] Sign what is actually sent (after any rewriting) and keep streaming
      responses streaming. The proxy's allowlist still applies.
- [ ] A signing crate would be a new dependency: ask in the issue before adding
      one. A hand-written SigV4 needs AWS's published test vectors as tests.
- [ ] Test in `crates/daemon/tests/features.rs` with a fake upstream: the
      `Authorization` header has the SigV4 shape, the agent token never reaches
      upstream, no credential reaches the sandbox env.
- [ ] Update `docs/decisions.md` and the README's model provider section.

## Acceptance criteria

- An agent on a Bedrock provider without an API key gets SigV4-signed requests
  made with the host's IAM credentials.
- No AWS credential shows up in a sandbox, log, event or API response.
- Existing Bedrock API-key providers work as before.

## Pointers

- `crates/daemon/src/proxy.rs`: `UpstreamAuth`, `Api::Bedrock`,
  `Api::BedrockControl`, the Bedrock upstream selection.
- `crates/core/src/provider.rs`, `crates/daemon/src/api/providers.rs`.
- `crates/daemon/tests/features.rs`: `providers_keep_keys_in_the_daemon`.
- `docs/decisions.md` ("Phase 2: Providers and drivers", "M1 Storage").
