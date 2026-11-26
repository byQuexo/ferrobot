import type { Cli, CliSpec, CliView } from "../api.ts";

export interface KnownCli {
  id: string;
  about: string;
  spec: Omit<CliSpec, "agents">;
  /** Where to create its token. */
  keyUrl?: string;
}

const debian = (id: string, about: string): KnownCli => ({
  id,
  about,
  spec: { install: { kind: "debian", package: id }, programs: [], account: null },
});

/** Ready-made CLIs: keyless Debian tools, and GitHub's with its own sign-in. */
export const KNOWN_CLIS: KnownCli[] = [
  debian("jq", "JSON"),
  debian("sqlite3", "SQLite databases"),
  debian("poppler-utils", "PDF text and pages (pdftotext)"),
  debian("pandoc", "Document conversion"),
  debian("imagemagick", "Images"),
  debian("ffmpeg", "Audio and video"),
  debian("graphviz", "Diagrams (dot)"),
  debian("zip", "Zip archives"),
  debian("unzip", "Unpack zip archives"),
  debian("tree", "Directory trees"),
  debian("python3-pip", "Python with pip"),
  debian("python3-venv", "Python virtual environments"),
  {
    id: "gh",
    about: "GitHub CLI, and git push over HTTPS",
    keyUrl: "https://github.com/settings/personal-access-tokens/new",
    spec: {
      install: { kind: "debian", package: "gh" },
      programs: ["gh"],
      account: {
        hosts: ["github.com", "api.github.com", "uploads.github.com"],
        env: ["GH_TOKEN", "GITHUB_TOKEN"],
        confirm: false,
        sign_in: {
          login: [
            "gh",
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--git-protocol",
            "https",
            "--web",
          ],
          token: ["gh", "auth", "token"],
        },
      },
    },
  },
];

export const knownCli = (id: string): KnownCli | undefined => KNOWN_CLIS.find((k) => k.id === id);

/** Words from a field that takes several, split by commas, spaces or new lines. */
export const splitList = (text: string): string[] => [
  ...new Set(
    text
      .split(/[\s,]+/)
      .map((w) => w.trim())
      .filter(Boolean),
  ),
];

/** How a CLI gets into the computers, in a few words. */
export function installLabel(cli: Pick<Cli, "install">): string {
  const i = cli.install;
  switch (i.kind) {
    case "debian":
      return `Debian package ${i.package}, in every computer`;
    case "npm":
      return `npm package ${i.package}`;
    case "download":
      return `Download from ${new URL(i.url).host}${i.sha256 ? "" : ", no checksum"}`;
    case "script":
      return "Install script";
  }
}

/** The CLI's account state, in a few words; empty without an account. */
export function accountLabel(cli: Pick<CliView, "account" | "has_token">): string {
  if (!cli.account) return "";
  return cli.has_token ? "Signed in" : "Needs a token";
}

/** The CLI as `PUT /v1/clis/{id}` takes it. */
export const specOf = (cli: Cli): CliSpec => ({
  install: cli.install,
  programs: cli.programs ?? [],
  account: cli.account ?? null,
  agents: cli.agents ?? [],
});
