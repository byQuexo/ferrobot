/** Vendor-run remote MCP servers that ferrobot can add in one step. Checked 2026-11-22. */
export interface CatalogEntry {
  /** The connector name (prefixes its tools). */
  name: string;
  title: string;
  url: string;
  about: string;
  /** `oauth`: sign in after adding; `token`: add its token in Vault; `none`: no sign-in. */
  auth: "oauth" | "token" | "none";
  kind:
    "work" | "notes" | "code" | "errors" | "data" | "cloud" | "money" | "docs" | "search" | "ai";
  /** A step needed before it works. */
  setup?: string;
  /** The server has no automatic client registration: Add opens the form for the operator's own
   * OAuth app with these filled in. */
  app?: { issuer: string; scopes: string[] };
}

export const CATALOG: CatalogEntry[] = [
  {
    name: "linear",
    title: "Linear",
    url: "https://mcp.linear.app/mcp",
    about: "Find, create and update issues, projects and comments.",
    auth: "oauth",
    kind: "work",
  },
  {
    name: "notion",
    title: "Notion",
    url: "https://mcp.notion.com/mcp",
    about: "Search pages and databases, create and edit pages.",
    auth: "oauth",
    kind: "notes",
  },
  {
    name: "atlassian",
    title: "Atlassian",
    url: "https://mcp.atlassian.com/v2/mcp",
    about: "Jira issues, Confluence pages, Bitbucket and Loom.",
    auth: "oauth",
    kind: "work",
  },
  {
    name: "gmail",
    title: "Gmail",
    url: "https://gmailmcp.googleapis.com/mcp/v1",
    about: "Search and read mail, manage labels and write drafts. It cannot send.",
    auth: "oauth",
    kind: "work",
    setup:
      "Google's official server, in Developer Preview. Needs your own Google OAuth app: see the Connectors and OAuth guide.",
    app: {
      issuer: "https://accounts.google.com",
      scopes: [
        "https://www.googleapis.com/auth/gmail.readonly",
        "https://www.googleapis.com/auth/gmail.compose",
        "https://www.googleapis.com/auth/gmail.modify",
      ],
    },
  },
  {
    name: "sentry",
    title: "Sentry",
    url: "https://mcp.sentry.dev/mcp",
    about: "Errors, performance and issue triage.",
    auth: "oauth",
    kind: "errors",
  },
  {
    name: "supabase",
    title: "Supabase",
    url: "https://mcp.supabase.com/mcp",
    about: "SQL, migrations, logs and edge functions.",
    auth: "oauth",
    kind: "data",
  },
  {
    name: "neon",
    title: "Neon",
    url: "https://mcp.neon.tech/mcp",
    about: "Postgres projects, branches, schema and SQL.",
    auth: "oauth",
    kind: "data",
  },
  {
    name: "cloudflare",
    title: "Cloudflare",
    url: "https://mcp.cloudflare.com/mcp",
    about: "The Cloudflare API: Workers, DNS, storage and more.",
    auth: "oauth",
    kind: "cloud",
  },
  {
    name: "stripe",
    title: "Stripe",
    url: "https://mcp.stripe.com",
    about: "Customers, payments, subscriptions and the Stripe docs.",
    auth: "oauth",
    kind: "money",
  },
  {
    name: "gitlab",
    title: "GitLab",
    url: "https://gitlab.com/api/v4/mcp",
    about: "Projects, issues, merge requests and CI.",
    auth: "oauth",
    kind: "code",
    setup: "A group owner may need to turn on MCP for your group first.",
  },
  {
    name: "github",
    title: "GitHub",
    url: "https://api.githubcopilot.com/mcp/",
    about: "Repos, issues, pull requests, Actions and code search.",
    auth: "token",
    kind: "code",
    setup: "Create a fine-grained personal access token on GitHub, then add it in Vault.",
  },
  {
    name: "huggingface",
    title: "Hugging Face",
    url: "https://huggingface.co/mcp?login",
    about: "Search models, datasets, Spaces and papers.",
    auth: "oauth",
    kind: "ai",
  },
  {
    name: "zapier",
    title: "Zapier",
    url: "https://mcp.zapier.com/api/v1/connect",
    about: "Actions in thousands of apps. Each call uses Zapier tasks.",
    auth: "oauth",
    kind: "work",
  },
  {
    name: "context7",
    title: "Context7",
    url: "https://mcp.context7.com/mcp",
    about: "Current docs and examples for code libraries.",
    auth: "none",
    kind: "docs",
  },
  {
    name: "deepwiki",
    title: "DeepWiki",
    url: "https://mcp.deepwiki.com/mcp",
    about: "Read and ask about public GitHub repos.",
    auth: "none",
    kind: "docs",
  },
  {
    name: "mslearn",
    title: "Microsoft Learn",
    url: "https://learn.microsoft.com/api/mcp",
    about: "Microsoft docs and code samples.",
    auth: "none",
    kind: "docs",
  },
  {
    name: "cloudflare-docs",
    title: "Cloudflare docs",
    url: "https://docs.mcp.cloudflare.com/mcp",
    about: "Search the Cloudflare developer docs.",
    auth: "none",
    kind: "docs",
  },
  {
    name: "exa",
    title: "Exa",
    url: "https://mcp.exa.ai/mcp",
    about: "Web search and page fetch, with a free limit.",
    auth: "none",
    kind: "search",
  },
];

/** Whether a catalog entry is already one of the operator's connectors. */
export const isAdded = (
  entry: CatalogEntry,
  connectors: { name: string; url: string }[],
): boolean => connectors.some((c) => c.url === entry.url || c.name === entry.name);
