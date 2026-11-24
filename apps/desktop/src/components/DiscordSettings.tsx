import type { JSX } from "preact";
import { useEffect, useState } from "preact/hooks";
import { X } from "lucide-preact";

import type { DiscordSettings as Settings, DiscordView } from "../api.ts";
import { useApp } from "../context.ts";
import { clockTime } from "../lib/format.ts";
import { PrefRow } from "./AppearanceSettings.tsx";
import { Button, IconButton, Switch } from "./ui.tsx";

const SNOWFLAKE = /^\d{15,21}$/;

/** The Discord bridge: allowed users write to agents from a channel and get their replies there. */
export function DiscordSettings(): JSX.Element | null {
  const { client, run, agents } = useApp();
  const [view, setView] = useState<DiscordView | null>(null);
  const [token, setToken] = useState("");
  const [users, setUsers] = useState("");
  const [channel, setChannel] = useState("");
  const [agent, setAgent] = useState("");

  const show = (v: DiscordView): void => {
    setView(v);
    setUsers(v.users.join(", "));
  };
  useEffect(() => {
    void run(() => client.discord()).then((v) => {
      if (v) show(v);
    });
    // Channel status changes as the daemon reads; look again now and then while open.
    const timer = setInterval(() => {
      client.discord().then(setView, () => undefined);
    }, 10_000);
    return () => {
      clearInterval(timer);
    };
  }, []);

  if (!view) return null;
  const list = [...agents.values()];
  const save = async (changes: Partial<Settings>): Promise<void> => {
    const saved = await run(() =>
      client.updateDiscord({
        enabled: view.enabled,
        users: view.users,
        bridges: view.bridges,
        ...changes,
      }),
    );
    if (saved) show(saved);
  };
  const saveToken = async (value: string | null): Promise<void> => {
    const saved = await run(() => client.setDiscordToken(value));
    if (saved) {
      show(saved);
      setToken("");
    }
  };
  const userIds = users
    .split(/[\s,]+/)
    .map((u) => u.trim())
    .filter(Boolean);
  const target = agent || list[0]?.id || "";
  const statusOf = (id: string): string => {
    const s = view.channels.find((c) => c.channel_id === id);
    if (s?.error) return s.error;
    if (s?.note) return s.note;
    if (s?.checked_at) return `Listening, read ${clockTime(s.checked_at)}`;
    return view.enabled ? "Starting…" : "Off";
  };

  return (
    <section class="settings-section">
      <ul class="pref-list">
        <PrefRow
          title="Bridge on"
          hint="Your messages in a bridged channel go to its agent as if you wrote them in ferrobot; the agent’s reply comes back to the channel."
        >
          <Switch
            checked={view.enabled}
            label="Bridge on"
            onChange={(v) => void save({ enabled: v })}
          />
        </PrefRow>
        <PrefRow
          title="Bot token"
          hint={
            view.has_token
              ? "Stored in the keychain of the daemon’s computer. Type a new one to replace it."
              : "From your app’s Bot page in Discord’s developer portal. Kept in the keychain."
          }
        >
          <span class="choice-custom">
            <input
              type="password"
              aria-label="Bot token"
              value={token}
              autoComplete="off"
              spellcheck={false}
              placeholder={view.has_token ? "Stored" : "Bot token"}
              onInput={(e) => {
                setToken(e.currentTarget.value);
              }}
            />
            <Button small disabled={!token.trim()} onClick={() => void saveToken(token)}>
              Save
            </Button>
          </span>
        </PrefRow>
        <PrefRow
          title="Your Discord user IDs"
          hint="Only these users reach your agents; everyone else in the channel is ignored. Several are separated by commas."
        >
          <span class="choice-custom">
            <input
              aria-label="Discord user IDs"
              value={users}
              spellcheck={false}
              autoComplete="off"
              placeholder="e.g. 123456789012345678"
              onInput={(e) => {
                setUsers(e.currentTarget.value);
              }}
            />
            <Button
              small
              disabled={
                userIds.join(",") === view.users.join(",") ||
                !userIds.every((u) => SNOWFLAKE.test(u))
              }
              onClick={() => void save({ users: userIds })}
            >
              Save
            </Button>
          </span>
        </PrefRow>
      </ul>

      <h3 class="pref-head">Channels</h3>
      <ul class="pref-list">
        {view.bridges.map((b) => (
          <PrefRow
            key={b.channel_id}
            title={`#${b.channel_id} → ${agents.get(b.agent_id)?.name ?? "a deleted agent"}`}
            hint={statusOf(b.channel_id)}
          >
            <IconButton
              label={`Stop bridging channel ${b.channel_id}`}
              onClick={() =>
                void save({ bridges: view.bridges.filter((x) => x.channel_id !== b.channel_id) })
              }
            >
              <X size={16} />
            </IconButton>
          </PrefRow>
        ))}
        <li class="pref-row hook-add">
          <input
            aria-label="Channel ID"
            value={channel}
            spellcheck={false}
            autoComplete="off"
            placeholder="Channel ID"
            onInput={(e) => {
              setChannel(e.currentTarget.value);
            }}
          />
          <select
            aria-label="Agent"
            value={target}
            onChange={(e) => {
              setAgent(e.currentTarget.value);
            }}
          >
            {list.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
          <Button
            small
            disabled={!SNOWFLAKE.test(channel.trim()) || !target}
            onClick={() => {
              void save({
                bridges: [...view.bridges, { channel_id: channel.trim(), agent_id: target }],
              }).then(() => {
                setChannel("");
              });
            }}
          >
            Add
          </Button>
        </li>
      </ul>
      <p class="hint">
        Turn on Developer Mode in Discord, then right-click a channel or your name and choose Copy
        ID. The bot needs the Message Content intent and access to the channel.
      </p>
    </section>
  );
}
