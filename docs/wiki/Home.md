A self-hosted platform for a team of AI agents. Every agent gets its own
sandboxed computer (a container today), a mailbox to talk to you and to the
other agents, a memory, and routines that run on a schedule. A Rust daemon
(`ferrobotd`) owns all state and a model proxy, so API keys never enter a
sandbox. A TypeScript runner inside each sandbox drives the model. A thin
desktop app, or any other client, talks to the daemon over a versioned HTTP
and WebSocket API.

The project is early. Large parts have never run against the real thing: see
[[Roadmap]] for what is done, what is partly done and what is untested.

## Guides

These pages are also in the desktop app under **Settings → Guides**.

- [[Getting Started|Getting-Started]]: the desktop download, or a build from source
- [[Model providers and keys|Providers]]: which models you can use and how keys are kept
- [[Connectors and OAuth|Connectors-and-OAuth]]: remote MCP servers, sign-in, your own OAuth app (Gmail example)
- [[CLIs]]: command-line tools in every agent's computer
- [[Tools and permissions|Tools-and-Permissions]]: tool groups, approval cards, Auto mode
- [[Routines and memory|Routines-and-Memory]]: scheduled work and notes that last
- [[Voice and calls|Voice-and-Calls]]: dictation, calls and call rooms
- [[Usage and Costs|Usage-and-Costs]]: token counts and cost estimates
- [[Troubleshooting]]: Docker, keys, the first start, macOS warnings

## How it works

- [[Architecture]]: daemon, model proxy, runner, sandboxes, store, MCP tools
- [[Sandbox Backends|Sandbox-Backends]]: Docker, local, Firecracker; sleep and archives

## Project

- [[Roadmap]]: status per phase and where help is wanted
- [[Ideas]]: what came up but is not scheduled
- [[Conventions]]: how code and commits are written here
- [[FAQ]]
