A self-hosted platform for a team of AI agents. Every agent gets its own
sandboxed computer (a container today), a mailbox to talk to you and to the
other agents, a memory, and routines that run on a schedule. A Rust daemon
(`ferrobotd`) owns all state and a model proxy, so API keys never enter a
sandbox. A TypeScript runner inside each sandbox drives the model. A thin
desktop app, or any other client, talks to the daemon over a versioned HTTP
and WebSocket API.

The project is early. Large parts have never run against the real thing: see
[[Roadmap]] for what is done, what is partly done and what is untested.

## Pages

- [[Getting Started|Getting-Started]]: the desktop download, or a build from source
- [[Architecture]]: daemon, model proxy, runner, sandboxes, store, MCP tools
- [[Providers]]: which models you can use and how keys are kept
- [[Sandbox Backends|Sandbox-Backends]]: Docker, local, Firecracker; sleep and archives
- [[Usage and Costs|Usage-and-Costs]]: token counts and cost estimates (in progress)
- [[Roadmap]]: status per phase and where help is wanted
- [[Ideas]]: what came up but is not scheduled
- [[Conventions]]: how code and commits are written here
- [[FAQ]]
