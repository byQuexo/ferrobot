Ideas that came up while building ferrobot. None is scheduled and none is a
promise; what is planned and how far it got is on the [[Roadmap]].

## Agents and sandboxes

- A file browser and a terminal in the agent's info panel.
- CLIs that need a login (`gh`, `vercel`, `stripe`) through a daemon-side
  proxy that swaps in the token, the way the model proxy does, for services
  without an MCP server.
- Per-tool scopes inside a connector (e.g. read-only GitHub tools), finer than
  today's on/off per group.

## Voice

- Faster answers in calls: reasoning switched off, or a fast model used only
  for calls.
- A setting for the pause that ends your turn in a call.
- Calls among agents without you, with a pop-up to join.

## Desktop and release

- A native macOS app on the same `/v1` API.
- A stable signature for development builds, so macOS stops asking for
  keychain access after every rebuild.
