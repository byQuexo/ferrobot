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

- It is your knowledge base. Agents with the **Memory** tool keep it on their
  own: when a conversation with you shows something lasting (your
  preferences, projects, people, tools, decisions, how you like answers),
  they save it with `remember` and `global: true`, update entries that
  changed and remove ones that are wrong or out of date, without asking.
  They never store keys, tokens or passwords.
- Every agent with the Memory tool gets the newest 50 entries in its
  instructions and sees changes on its next turn; older ones it finds with
  `recall`. They come marked as notes, not instructions.
- Agents can change each other's entries, never yours: what you add (the same
  key overwrites) stays as you wrote it until you change or remove it.
- `recall` searches an agent's own memory and the global memory together.

Good uses besides facts about you: things every agent should know, such as
team names, a deploy day or where the docs live.

## Memory in an Obsidian vault

Instead of the built-in memory, memories can live as Markdown notes in your
[Obsidian](https://obsidian.md) vault, so you read, link and edit them there.
In **Settings → Global memory**, enter the vault's folder (for example
`~/Obsidian/Notes`) and click **Use**.

- Notes go in a `ferrobot` folder inside the vault: one folder per agent
  (named after the agent; it follows a rename) and `Global memory` for the
  shared one. Each memory is one note, named after its key, with the key and,
  for global notes, the agent that wrote it in the frontmatter, and the tag
  `ferrobot`.
- `remember`, `recall` and `forget` and the Memory views then read and write
  these notes. A note you add or edit in Obsidian counts too; a note without
  frontmatter takes its file name as the key and counts as yours, so agents
  cannot overwrite it.
- Memories stored so far are copied in when you switch (notes with the same
  key are kept). **Stop using it** goes back to the built-in memory, which
  still holds what it had before the switch; notes made since stay in the
  vault.
- Search in the vault matches words and word beginnings, best match first.
- Deleting an agent leaves its notes in the vault.
- Agents only reach the vault through these tools: no agent can name a path,
  and keys become plain file names.
