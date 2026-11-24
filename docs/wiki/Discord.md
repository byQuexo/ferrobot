# Discord bridge

Write to your agents from a Discord channel and get their replies there. Each
bridged channel talks to one agent. Only the Discord users you allow reach it;
their messages arrive as if you wrote them in ferrobot (marked "via Discord"),
and the agent's reply comes back as a reply to that message.

ferrobot reads the channel every few seconds through Discord's API with your
bot's token, so nothing has to be reachable from the internet. The token is
kept in the keychain of the daemon's computer; agents never see it.

## Set it up

1. **Create a bot.** In the [Discord developer
   portal](https://discord.com/developers/applications), create an
   application, open **Bot**, and copy its token (**Reset Token** shows a new
   one).
2. **Let it read messages.** On the same page, under *Privileged Gateway
   Intents*, turn on **Message Content Intent**. Without it Discord sends the
   bot empty messages.
3. **Invite it to your server.** Under **OAuth2 → URL Generator**, tick the
   `bot` scope and the permissions **View Channels**, **Send Messages**,
   **Read Message History** and **Add Reactions**, open the generated link
   and pick your server.
   A private channel needs the bot (or its role) added to it.
4. **Copy the IDs.** In Discord's settings, under *Advanced*, turn on
   **Developer Mode**. Right-click the channel → **Copy Channel ID**, and your
   own name → **Copy User ID**.
5. **In ferrobot**, open **Settings → Discord**: paste the bot token and
   **Save**, enter your user ID and **Save**, add the channel with the agent
   it should talk to, and turn on **Bridge on**.

Write in the channel; within a few seconds the agent gets your message (the
bot reacts with 👀), and its answer shows up as a reply (the 👀 turns into ✅).
In ferrobot the message carries a Discord chip with who wrote it. Each channel shows when it was last read, or
what went wrong (a wrong token, a channel the bot cannot see).

## Good to know

- Only allowed users count. Messages from anyone else in the channel, and from
  bots, are ignored. Several people can be allowed; their messages all arrive
  as yours, so only allow people you would give your ferrobot to.
- Messages sent before the bridge started (or while ferrobot was off) are not
  replayed.
- Attachments arrive as links. A long reply is split into a few Discord
  messages; anything beyond that stays in ferrobot.
- The agent can also post to its channel on its own (`send_to_discord`), for
  example progress on long work or a routine's result; it reaches only the
  channels bridged to it.
- Replies never ping anyone (`@everyone` and mentions are turned off).
- Question and approval cards are still answered in the app. To hear about
  them in Discord, add an outgoing [[Webhooks]] entry for "Cards waiting for
  you".
- Risky steps follow the same rules as in the app: a card unless Auto mode is
  on, dangerous ones refused.
