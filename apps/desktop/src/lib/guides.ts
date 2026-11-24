/** A wiki page shown under Settings → Guides; `page` is its file name in docs/wiki without `.md`. */
export interface GuideInfo {
  page: string;
  title: string;
  about: string;
}

/** The user guides, in reading order. Project pages (Architecture, Roadmap, ...) stay on the wiki. */
export const GUIDES: readonly GuideInfo[] = [
  {
    page: "Getting-Started",
    title: "Getting started",
    about: "Install, Docker and your first agent.",
  },
  {
    page: "Providers",
    title: "Model providers and keys",
    about: "Which models you can use; keys in Vault.",
  },
  {
    page: "Connectors-and-OAuth",
    title: "Connectors and OAuth",
    about: "Give agents tools of remote services, Gmail included.",
  },
  { page: "CLIs", title: "CLIs", about: "Command-line tools in every agent’s computer." },
  {
    page: "Tools-and-Permissions",
    title: "Tools and permissions",
    about: "What each agent may do, and what needs your approval.",
  },
  {
    page: "Routines-and-Memory",
    title: "Routines and memory",
    about: "Scheduled work and what agents remember.",
  },
  {
    page: "Voice-and-Calls",
    title: "Voice and calls",
    about: "Dictation, calls and call rooms.",
  },
  {
    page: "Usage-and-Costs",
    title: "Usage and costs",
    about: "Tokens and cost estimates per agent and model.",
  },
  {
    page: "Traces",
    title: "Traces",
    about: "A turn’s steps on a timeline, and export to Phoenix or Langfuse.",
  },
  {
    page: "Webhooks",
    title: "Webhooks",
    about: "Messages from GitHub, n8n or Zapier in; events out to Discord or Slack.",
  },
  {
    page: "Discord",
    title: "Discord bridge",
    about: "Write to your agents from a Discord channel.",
  },
  {
    page: "Troubleshooting",
    title: "Troubleshooting",
    about: "Docker, keys, the runner image and macOS warnings.",
  },
  { page: "FAQ", title: "FAQ", about: "Short answers to common questions." },
];

export type WikiPart = { text: string } | { page: string; label: string };

const WIKI_LINK = /\[\[([^\]|\n]+)(?:\|([^\]\n]+))?\]\]/g;

/** Splits GitHub wiki links, `[[Page]]` and `[[Text|Page]]`, out of text; spaces in a page name become dashes. */
export function splitWikiLinks(text: string): WikiPart[] {
  const out: WikiPart[] = [];
  let last = 0;
  for (const m of text.matchAll(WIKI_LINK)) {
    if (m.index > last) out.push({ text: text.slice(last, m.index) });
    const label = (m[1] ?? "").trim();
    const target = (m[2] ?? label).replace(/#.*$/, "").trim();
    out.push({ page: target.replace(/\s+/g, "-"), label });
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}
