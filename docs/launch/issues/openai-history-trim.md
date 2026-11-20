# Runner: trim the OpenAI-compatible driver's history

**Labels:** `good first issue`, `area: runner`

## Context

The `openai` driver (providers `openai`, `gemini`, `openrouter`, `ollama`,
`openai_compatible`) keeps a session's whole conversation in a JSON file and
sends all of it on every model call. Nothing trims it, so a long-lived agent
eventually exceeds its model's context window and every turn fails. See the
`shortcut:` comment above `#save` in `runner/src/drivers/openai.ts`.

## Tasks

- [ ] Before a turn's first model call, drop the oldest messages of the loaded
      history once it is over a budget.
- [ ] Cut only right before a `user` message, so an `assistant` message with
      `tool_calls` is never separated from its `tool` results (upstreams reject
      that). Late messages (`#late`) are pushed after the tool results, so this
      holds for them too.
- [ ] Measure the budget in characters as a stand-in for tokens; one constant
      is enough to start. Keep the system message and the current turn whole.
- [ ] Tests in `runner/test/openai.test.ts`: a long history is trimmed at a
      `user` boundary, tool calls stay paired, a short history is untouched.
- [ ] Remove the `shortcut:` comment.

Summarising dropped turns instead of dropping them is a separate, later issue.

## Acceptance criteria

- `npm run lint && npm run typecheck && npm test` pass in `runner/`.
- A session over the budget sends a shorter, valid message list.
- No new npm dependency.

## Pointers

- `runner/src/drivers/openai.ts`: `ChatMessage`, `#turn`, `#late`, `#load`,
  `#save`.
- `runner/test/openai.test.ts`.
- `docs/progress.md` ("Phase 2", Open).
