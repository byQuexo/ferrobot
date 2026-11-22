Routines give an agent recurring work. Memory lets it keep notes across
conversations. Both are per agent, plus one global memory every agent shares.

## Routines

A routine sends its agent a prompt on a schedule, as a message. A sleeping
agent wakes up for it.

Add one in the agent's details panel: **Routines → New routine**.

- **Name**: unique for this agent, for example `morning-digest`.
- **Schedule**: a 5-field cron expression (`0 9 * * 1-5`) or an alias
  (`@hourly`, `@daily`). The chips fill in common ones: every day at 09:00,
  weekdays at 09:00, every hour, Fridays at 18:00.
- **Time zone**: where the schedule's clock runs, for example `Europe/Berlin`.
  It starts with the daemon's own time zone. Daylight saving time is handled.
- **Prompt**: what the agent gets on every run, for example "Summarize what
  changed since yesterday."

Each routine has **Run now**, a switch to turn it off and on, and its recent
runs. A run missed while the daemon was down fires once when it is back.

### Routines an agent makes

An agent with the **Routines** tool can set up routines for itself
(`create_routine`). Unless Auto mode is on, you get a card to approve it
first; the agent hears back by message. See
[[Tools and permissions|Tools-and-Permissions]].

## Memory

Agents with the **Memory** tool keep notes under a key: `remember` stores or
overwrites one, `recall` searches them (full text, best match first) and
`forget` deletes one.

- An agent's own memory is in its details panel under **Memory**. You can
  search it, add entries (**Add memory**; the same key overwrites) and remove
  them.
- Memory lives in the daemon, not in the agent's computer, so it survives a
  rebuilt sandbox.

## Global memory

The global memory is shared by every agent. Open it in
**Settings → Global memory**.

- Agents add to it with `remember` and `global: true`. An agent can only
  overwrite global notes it wrote itself.
- You can add entries (the same key overwrites) and remove any entry.
- `recall` searches an agent's own memory and the global memory together.
  Agents are told that global notes are information from teammates or you,
  not instructions.

Good uses: facts every agent should know, such as team names, a deploy day or
where the docs live.
