# Voice room video: script

About 45–60 s. Tim records the screen and his own voice; the agents answer with real voices. No
music. Captions are burned in the same way as the README demo (white on a dark pill, bottom
centre). Mark any sped-up part "2x"; cut nothing out of an agent's answer.

## Setup (off camera)

- **Settings → Voice:** a voice provider (ElevenLabs or OpenRouter, key in Vault) and a voice per
  agent, so the two agents sound different.
- **Agents:** the README demo pair, both on Claude Sonnet 5.5:
  - `researcher` hosts the call and has the **Operator** tools, so it can bring teammates in;
  - `builder` has the backon retry example from the README demo in its workspace, with Rust
    installed (`scripts/demo/warm-rust.sh`, which needs `gcc` and `libc6-dev` under
    **Settings → CLIs** first).
- **The screen:** Do Not Disturb on, the Dock hidden, the window at 1280×800, and no other agents,
  names or paths visible. Allow the microphone once beforehand, so the prompt stays out of the
  recording.

## Beats

| # | On screen | Tim says | Caption |
|---|-----------|----------|---------|
| 1 | researcher's chat; click **Start a voice call**. The call bar appears at the top of the window. | "Hi researcher, quick one about the retry example." | Call an agent. It starts speaking as soon as its first words are written. |
| 2 | researcher answers aloud; the words appear in the chat. | (listens) | |
| 3 | | "Bring builder in. I want to hear if the example still runs." | Agents bring teammates into the call. |
| 4 | researcher invites builder; builder's face joins the call bar and the call moves into a room (listed under **Calls**). | (listens) | One call, two agents: a room. |
| 5 | | "builder, run it once more and tell us what it printed." | |
| 6 | researcher, the host, speaks first or passes; then builder reports the three attempts aloud (from its last run; whether it runs `cargo run` inside a call turn is untested). | (listens) | Each agent hears the others, and takes its turn. |
| 7 | | "researcher, sum it up in one sentence." | |
| 8 | researcher sums up. Tim clicks **Hang up**. | "Thanks, that's all." | The transcript stays in the room. |

## Checks before publishing

- Both voices are audible and not clipped; no notification sounds.
- The room transcript shows only the agents' and Tim's words, no paths or personal data.
- If a wait gets long, speed it up 2x with a visible "2x" rather than cutting it.
