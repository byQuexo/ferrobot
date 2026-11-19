# Progress

Status per roadmap phase (`docs/roadmap.md`). Decisions are in
`docs/decisions.md`.

## M1 Storage layer — done (`feature/m1-storage`)

**Done**

- `object_store` 0.14.2 (MIT/Apache-2.0) behind `--storage-url` /
  `FERROBOT_STORAGE_URL`; default is the local data dir, `s3://…` covers
  S3 and R2, `file://` and `memory://` work too (`crates/daemon/src/storage.rs`).
- Docker snapshot home archives go through the store
  (`DockerConfig::snapshot_store`) instead of a fixed directory.
- No MinIO, no AGPL crate added.

**Tested**

- Put/get/delete on the local FS, on a `file://` URL and on `memory://`;
  an `s3://` URL builds offline (`storage::tests`).
- Docker snapshot → restore → delete through the store:
  `cargo test -p ferrobot-sandbox --features docker-tests`.

**Open**

- Not run against a real S3 or R2 bucket (needs credentials).
- Home archives are buffered in memory and sent as one PUT (S3 limit 5 GiB);
  see the `shortcut:` comment in `docker.rs`.

## Phase 2: Providers and drivers (`feature/providers`)

**Done**

- Provider kinds: Anthropic, OpenAI, Gemini, Bedrock, Vertex, OpenRouter,
  Ollama, generic OpenAI-compatible (`crates/core/src/provider.rs`).
- Migration v7: `providers` table, `agents.provider_id` and `agents.model`.
- `/v1/providers` CRUD; keys only in the secret store (OS keychain by
  default, `--secret-store file` for CI), never returned (`has_key` only).
- Agents pick provider and model (`POST`/`PATCH /v1/agents`); a change
  rebuilds the sandbox with the new env, disk kept. Agent-created agents
  inherit the creator's choice.
- Model proxy routes per agent: upstream from the provider, credential
  injected per kind (`x-api-key`, Bearer key, Bedrock API key, gcloud token).
- Runner: new OpenAI-compatible driver (streaming, tool loop with `bash`,
  `read_file`, `write_file` and the ferrobot MCP tools, permission policy,
  persisted sessions, abort). No new npm dependency.
- Desktop: "Model providers" dialog (key icon in the sidebar), provider and
  model fields in the agent dialog.

**Tested**

- Rust: provider kinds/validation, store CRUD + in-use guard, migration v7
  upgrade, secret stores (memory, file with mode 0600), and a daemon test
  per kind that checks sandbox env (no secret inside) and the credential the
  upstream receives (`features.rs`: `providers_keep_keys_in_the_daemon`).
- Runner: 13 new tests for the driver (fake fetch and MCP).
- End to end by hand: real `ferrobotd` (local backend) + real runner with the
  openai driver + a fake OpenAI-compatible server. A message ran a `bash`
  tool call and streamed the answer back; the upstream saw the provider key,
  the key never appeared in the sandbox directory or the daemon log.
- Bedrock and Vertex request shapes were checked by capturing the bundled
  Claude binary's requests against a local server (no AWS/GCP calls).

**Open**

- Not run against any real provider (needs keys): OpenAI, Gemini,
  OpenRouter, Ollama, Bedrock, Vertex, Anthropic via a provider.
- Gemini's OpenAI-compatible endpoint with our tool schemas is untested
  (schemas are flattened, `$defs` inlined, for that reason).
- Bedrock supports API keys only (no IAM/SigV4); Vertex needs `gcloud` logged
  in on the daemon host.
- OpenAI driver: history is never trimmed (`shortcut:` comment); no audit log
  lines like the Claude driver writes.
- Runner image must be rebuilt for Docker to have the new driver.
- The desktop dialogs were not looked at in a running window.
