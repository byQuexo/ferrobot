Every agent has a tool profile: which groups of tools it may use and which
connectors it has. You set it per agent. Agents that create agents can pass
on at most what they have, and some actions wait for your approval.

## Tool groups

Set them in the agent's dialog (**New agent** or **Edit agent**), under
**Tools**. A new agent you create gets every group unless you switch some
off.

| Group | What it allows |
| --- | --- |
| Team | Message teammates, give them tasks, and call them without you |
| Memory | Remember and recall notes |
| Operator | Ask you questions, share files, react to your messages, bring teammates into calls, post to its Discord channel, read the guides |
| Create agents | Ask to create new agents |
| Add connectors | Ask you to add connectors |
| Routines | Set up its own routines |
| Shell | Run commands and every CLI on its computer |
| Files | Read and edit files |
| Web | Fetch pages and search the web |

- The first six are ferrobot's own tools. The daemon hides the ones outside
  the profile and refuses them if an agent calls them anyway.
- Shell, Files and Web are the model harness's built-in tools. The runner
  turns them off per driver.
- **Web** only shows for providers whose harness has web tools (Anthropic,
  Bedrock, Vertex, Codex, Gemini CLI).
- With **Shell** on, an agent can still read files and reach the web through
  commands.
- Changes apply from the agent's next turn.

Connectors are switched on per agent in the same list. See
[[Connectors and OAuth|Connectors-and-OAuth]].

## Subagents

An agent can split work into parallel parts without a new teammate. Claude
uses the SDK's Agent tool; OpenAI-compatible providers get ferrobot's
`subagent` tool. It runs a task with the same model and tools and a fresh
history, at most 20 model calls, and returns the answer. Several in one step
run in parallel; their steps go through the same checks and show in
Activities.

- Subagents are off while Shell, Files or Web is off, since they run their
  own tools. Where Codex or Gemini CLI offer their own, the same rule applies.
- Claude Code (your own login) has none.

## Files and reactions in the chat

- **Attach files** with the paperclip, by dropping them on the window, or by
  pasting an image. They go into the agent's workspace folder `uploads/`
  (at most 25 MB each, `POST /v1/agents/{id}/uploads`), waking the agent if
  it sleeps. The message gets a line per file, like
  `Attached: uploads/photo.png (image, 120.0 KB)`, and the agent opens them
  with its file tools.
- **Reactions**: an agent with **Operator** can acknowledge your message with
  one emoji (`react`) instead of a reply. It shows on your message.
- **Discord**: an agent with **Operator** and a [[Discord]] channel bridged to
  it can post there on its own (`send_to_discord`), for example progress on
  long work or a routine's result.

## Agents that create agents

An agent with **Create agents** can ask for a new teammate (`create_agent`).

- The new agent gets at most its creator's tool groups and connectors. By
  default it gets exactly those, and the same provider and model. The daemon
  checks this on every path, Auto mode included, so a request can never widen
  them.
- The name must be valid and free, and the agent limit applies: 10 agents by
  default (`FERROBOT_MAX_AGENTS`), for agents you create too.
- **Names for new agents** in **Settings → Agents** sets a naming convention
  in your own words, for example "Greek gods". Agents that may create agents
  get it with their instructions. Empty means short names that fit the role,
  like `scout`.

### Changing a created agent's tools

With `set_agent_tools`, an agent can change the tool groups and connectors of
agents it created, at most its own. Narrowing applies at once. Widening needs
your approval, unless Auto mode is on.

## Approval cards

Without Auto mode, these requests show you a card first:

- **Create an agent**: the card shows the name, role, instructions, tools and
  connectors, so nothing is approved unseen. Click **Approve** or
  **Decline**. The agent's tool call waits for your answer for up to 5
  minutes (`FERROBOT_APPROVAL_TIMEOUT`, 300 s); no answer counts as declined.
- **Create a routine** (`create_routine`): the same rule, but the agent does
  not wait. A message tells it whether you approved.
- **Add a connector** (`add_connector`): always a card, Auto mode or not. It
  never asks for a key: a token goes in Vault, where you type it yourself.
- **Give an agent more tools** (`set_agent_tools`): the card says which agent
  wants to give which of its agents more tools, and lists what is new. The
  agent does not wait; a message tells it whether you approved. The card stays
  open until you answer.
- **A risky step**: see [Risky steps](#risky-steps) below. The step waits for
  your answer, like a new agent does.

A card appears in the chat of the agent you talk to: for an agent made by
another agent, that is the chat of the agent at the top of the chain. A card
can be answered once.

## Risky steps

A small rule-based classifier checks every shell command an agent runs (in any
driver) and every connector tool it calls, before it happens:

- **Safe** steps run: building, testing, reading, writing files in the
  workspace, most commands.
- **Risky** steps get a card unless Auto mode is on: deleting a folder
  (`rm -r`), `git push`, `git reset --hard`, `git clean -f`, sending data with
  `curl`/`wget` (POST, `-d`, uploads), `ssh`/`scp`/remote `rsync`, `sudo`,
  killing processes, sending mail, and connector tools whose names act on the
  outside world (send, post, create, update and similar; reading, listing and
  searching stay safe). The card says what the step does and why it asks.
- **Critical** steps always get a card, even in Auto mode: connector tools
  that move money (pay, payment, payout, transfer, refund, charge) or delete
  or cancel something (delete, remove, trash, cancel, destroy, purge),
  force-pushing (`git push --force`, `-f`, `+branch`) and publishing a package
  (`npm`/`cargo`/`pnpm`/`yarn`/`twine`/`gem publish`). The card says "a very
  risky step".
- **Dangerous** steps are always refused, Auto mode or not: deleting `/` or
  the home folder, formatting a disk, writing to a raw device, piping a
  download into a shell, a fork bomb.

The classifier is plain rules, so it is fast, free and predictable, and it
misses clever variations. The sandbox stays the real boundary.

## Auto mode

Turn it on in **Settings → Agents → Auto mode**. Agents then create agents,
their own routines and give their agents more tools without asking, and risky
steps run without a card. Only critical steps (money, deleting in a connected
service, force-pushing, publishing) still ask; dangerous ones stay blocked. A new agent or a tool
change leaves a notice in the chat instead of a card; a new routine shows up
under the agent's **Routines**. The agent limit, the tool limits above and
your keys stay protected.
