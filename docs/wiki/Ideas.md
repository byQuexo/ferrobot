Ideas that come up while building ferrobot, before they are planned. None is
a promise.

Everything that was listed here is now on the [[Roadmap]] (items 7 to 10 and 13):
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
