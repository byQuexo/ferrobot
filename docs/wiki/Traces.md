# Traces

Every reply has a steps line in its footer. Click it to open the turn's
**trace**: one timeline of its model calls and tool calls, with how long each
took, tokens, the slowest step, failures and a search. Model calls come from
the model proxy, or for Claude Code agents from `claude`'s own output.

## Export to Phoenix, Langfuse or another backend

**Settings → Traces** sends each turn's trace to an OpenTelemetry backend
when the turn ends. The turn is the root span (named after the agent), with
one span per model call and tool call below it. Spans carry both
[OpenInference](https://github.com/Arize-ai/openinference) attributes
(`openinference.span.kind`, `llm.model_name`, `llm.token_count.*`,
`tool.name`) and OpenTelemetry GenAI ones (`gen_ai.request.model`,
`gen_ai.usage.*`), so Phoenix and Langfuse both show them as LLM and tool
steps. Each agent's turns share a session (`session.id` is the agent's id).

Export is off by default. By default only names, times, tokens, outcomes and
errors leave this computer; **Include content** also sends tool inputs
(commands, file paths and contents) and the reply. Keys go to the keychain of
the daemon's computer and are never shown again.

The status line under **Save** says how the last export went, e.g. a wrong
key shows as "the endpoint answered 401".

### Phoenix

1. Start Phoenix, for example with Docker:
   `docker run -p 6006:6006 arizephoenix/phoenix:latest`
2. In **Settings → Traces**, pick **Phoenix**. The endpoint is filled in:
   `http://localhost:6006/v1/traces`.
3. Optional: a **Project** name (otherwise Phoenix's default project).
4. Turn on **Send traces**, send an agent a message, and open
   http://localhost:6006.

Phoenix Cloud: use `https://app.phoenix.arize.com/s/<your space>/v1/traces`
and your Phoenix API key (sent as `Authorization: Bearer <key>`). The key is
also needed for a self-hosted Phoenix with authentication on.

### Langfuse

1. In your Langfuse project, open **Settings → API Keys** and create a key
   pair.
2. In **Settings → Traces**, pick **Langfuse**. The endpoint is filled in for
   Langfuse Cloud EU: `https://cloud.langfuse.com/api/public/otel/v1/traces`.
   For the US region use `https://us.cloud.langfuse.com/api/public/otel/v1/traces`;
   for a self-hosted Langfuse (v3.22 or later),
   `https://<your host>/api/public/otel/v1/traces`.
3. Paste the **public key** (`pk-lf-…`) and **secret key** (`sk-lf-…`). They
   are sent as basic auth.
4. Turn on **Send traces** and send an agent a message. Turns show up under
   **Tracing**, named after the agent, with model calls as generations.

### Any other OpenTelemetry backend

Pick **Other** and enter the backend's OTLP/HTTP traces URL, usually ending in
`/v1/traces` (port 4318 for an OpenTelemetry Collector, Jaeger or Grafana
Tempo). If the backend needs a key, give the header's name (for example
`Authorization` or `x-honeycomb-team`) and its value. Traces are sent as
protobuf (`application/x-protobuf`), which every OTLP/HTTP receiver accepts.

## What is not in a trace yet

- Approval waits and work handed to other agents (planned).
- Turns from before an agent ran on this version have no Claude Code model
  calls.
