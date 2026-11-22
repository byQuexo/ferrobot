Every agent has a tool profile: which groups of tools it may use and which
connectors it has. You set it per agent. Agents that create agents can pass
on at most what they have, and some actions wait for your approval.

## Tool groups

Set them in the agent's dialog (**New agent** or **Edit agent**), under
**Tools**. A new agent you create gets every group unless you switch some
off.

| Group | What it allows |
| --- | --- |
| Team | Message teammates and give them tasks |
| Memory | Remember and recall notes |
| Operator | Ask you questions, share files, bring teammates into calls, read the guides |
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
- **Add a connector** (`add_connector`): always a card, Auto mode or not. For
  a token it has a hidden field; the token goes straight to Vault.
- **Give an agent more tools** (`set_agent_tools`): the card says which agent
  wants to give which of its agents more tools, and lists what is new. The
  agent does not wait; a message tells it whether you approved. The card stays
  open until you answer.

A card appears in the chat of the agent you talk to: for an agent made by
another agent, that is the chat of the agent at the top of the chain. A card
can be answered once.

## Auto mode

Turn it on in **Settings → Agents → Auto mode**. Agents then create agents,
their own routines and give their agents more tools without asking. A new
agent or a tool change leaves a notice in the chat instead of a card; a new
routine shows up under the agent's **Routines**. The agent limit, the tool limits above and your keys stay
protected.
