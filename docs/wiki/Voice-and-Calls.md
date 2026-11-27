Talk to your agents instead of typing: dictate messages, or call an agent and
hear its replies read aloud. Two or more agents in a call share a room.

## Set it up

1. Open **Settings → Voice** and pick a **Provider**:
   - **ElevenLabs**: speech, live transcription, your account's voices and a
     speed setting.
   - **OpenRouter**: speech and transcription models; no live transcription.
2. Paste the provider's API key in the **API key** row and click **Save**.
   Like model keys, it stays in the daemon's keychain (Vault lists it too)
   and is never shown again. With ElevenLabs your voices load right away.
3. Optional: pick a **Default voice**, a **Speech model**, a **Transcription
   model** and a **Language** (or **Detect automatically**).

Every change saves by itself. Switching providers clears the voices and models
you picked for the old one, so the page asks first. **Voice in chats**
switches it all off and on. Off hides the call and microphone buttons;
settings and keys are kept.

## Voices per agent

Under **Voices** you give each agent its own voice, so agents in a call sound
different. In a chat, the sliders button (**Voice, language and speed**)
changes the voice, language and speed for that agent on the fly; the play
button (**Test voice**) plays a short sample.

## Dictate

The microphone button in the message box (**Dictate a message**) turns speech
into text you can edit before sending. With ElevenLabs the words appear while
you speak; with OpenRouter the recording is transcribed when you stop.

## Call one agent

Click the phone button in an agent's chat (**Start a voice call**).

- The agent's replies are read aloud. It answers like on the phone: short
  spoken sentences, no lists or links.
- With ElevenLabs the call listens hands-free. With OpenRouter, replies are
  read aloud and you dictate with the microphone.
- The call bar at the top of the window shows everyone in the call and your
  microphone level, and has **Mute** and **Hang up**. It stays while you
  switch chats, Settings or Usage.
- With ElevenLabs the bar also has the speaking speed (**1×**; click for the
  next step, 0.7× to 1.2×). It is the same speed as in Settings and the chat's
  sliders, so every agent speaks at it, within a sentence or two.
- The speaker button (**Call volume**) opens a small slider for how loud the
  agents speak; at 0 they are muted. This computer remembers it, and
  **Settings → Voice → Calls** has the same slider and **Push to talk**.
- **Push to talk** in the bar switches from always listening to talking only
  while you hold **Hold to talk** or Space (outside text fields). Words count
  once you let go and pause briefly; **Mute** still wins. This computer
  remembers the choice.
- **Pause that ends your turn** (**Settings → Voice → Calls**, ElevenLabs):
  how long you can stop talking before agents answer, 0.5 to 3 seconds
  (default 1.5). It applies from the next call.
- **Model in calls** (the chat's sliders button): a quicker model this agent
  uses only while it is in a call, such as a small model for spoken replies.
  It applies where the request names the model, so not to Bedrock, Vertex or
  Gemini agents.
- Longer work: the agent says so and does it after the call, in its chat.
- An agent cannot show question cards during a call; it asks out loud.

## Call rooms

The agent you called hosts the call. It can bring a teammate in
(`invite_to_call`), or create a specialist first and then invite it. Once a
second agent joins, the call becomes a room:

- Up to four agents per call.
- Everyone hears every line. Agents take the floor one at a time, host first,
  then in the order they joined. Each one speaks or passes.
- After four agent lines in a row the room waits for you.
- Rooms are listed under **Calls** in the sidebar with their transcript. Room
  lines stay out of the agents' chats.

## Calls among agents

An agent can call one to three teammates without you (`call_agents`, in the
Team tool group) to talk something through together. They talk by the room
rules above, in writing, with the call-among-agents note. Agents get the tool
only while voice is on with a key, because without voice you could not join.

- A pop-up says who started the call and what about, with **Join** and
  **Open**. Joining makes it your voice call in the same room; the agent that
  called hosts it. The room's header has **Join** too while the call runs.
- It ends by itself once nobody has anything to add, after at most twelve
  agent lines in a row, or when the agent that called hangs up.
- The room is listed under **Calls** with its transcript; its lines stay out of
  the agents' chats.

## Group chats

A group is a named room you keep: **Groups → +** in the sidebar, a name and
two or more agents.

- Write once and every member reads it. They answer by the room rules above,
  one at a time, each replying or passing, without the voice-call note.
- **Call** in the group's header calls every member (up to four). The group
  is the call's room, so the call's lines land in it; hanging up keeps the
  group. An agent invited into the call joins the group.
- **Edit** renames the group, changes its members or deletes it with its
  messages. Members are fixed while the group is in a call.
- Who is due next lives in memory: after a daemon restart, write again.
- API: `/v1/groups` (list, create), `PATCH`/`DELETE /v1/groups/{id}`,
  `POST /v1/groups/{id}/messages`, `POST /v1/groups/{id}/call`; the
  transcript is `GET /v1/rooms/{id}`.

## Hang up

- **Hang up** ends the call for everyone. The turn holding the floor is cut
  off and waiting turns are dropped.
- An agent can hang up too (`hang_up`). It takes effect when its turn ends, so
  its goodbye is still heard. The host ends the call; an invited agent only
  leaves.

## What has been tested

ElevenLabs ran live in the desktop window: calls, rooms, streamed speech and
live transcription. OpenRouter voice has only run against a fake upstream.
