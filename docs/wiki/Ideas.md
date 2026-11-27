Ideas that come up while building ferrobot, before they are planned. None is
a promise.

Everything that was listed here is now on the [[Roadmap]] (items 7 to 10 and 12):
the file browser and terminal in the Computer tab, logins for CLIs and finer
connector scopes, faster calls and calls among agents, a stable signature for
development builds, and a native macOS app.

## Voice over Discord

Talk to an agent from your phone through the [[Discord]] bridge.

- **Voice messages in.** A Discord voice message reaches the agent as text,
  transcribed with the voice provider from Settings → Voice. Today the agent
  gets only a link to the audio file.
- **Voice replies.** The agent's reply also comes back as a Discord voice
  message (OGG/Opus with a waveform and duration).
- **Ping me.** An option to let the bridge mention you when a card waits for
  you, so your phone notifies you; replies never ping today.
- **Live calls in a voice channel.** The bot joins a voice channel on your
  server and talks with you there by the call rules. Still outbound only, but
  it needs Discord's live gateway and voice connection, Opus and Discord's
  end-to-end encryption for calls (DAVE), so new dependencies. A bot cannot
  ring you in a private call; the closest is joining a channel and pinging you.

## Flows

Steps you set up once and that run on their own: an agent's turn, a message
to another agent, a card that waits for you, a condition, a post to a
webhook, started by a trigger (a webhook, a schedule, a Discord message, a
finished task) or by an agent. A linear proof of concept (agent turns,
conditions and messages, run by hand from Settings) was built for 0.1.11 and
left out: it was not the right shape, and the full idea is too big for now.

## Phone

- **Twilio connector, so an agent can call you.** Settings → Phone takes a
  Twilio account (its auth token in the keychain, like every key) and the
  number to call. An agent calls your phone when it needs you: a card that
  waits, a failed task, something you asked it to watch. Only your own
  number, and with a limit on how often it rings.
- **A spoken notice first.** Twilio reads a short text aloud and hangs up.
  This needs no public address, because the daemon sends the words with the
  call request.
- **A real conversation later.** Talking back needs Twilio to reach the
  daemon (its speech-to-text callbacks or a live audio stream over a
  WebSocket), so a public address: a tunnel, or ferrobot in the cloud
  (roadmap item 11). The call would follow the call rules of the app's voice
  calls.
