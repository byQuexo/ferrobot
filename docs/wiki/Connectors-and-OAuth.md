A connector is a remote MCP server whose tools you lend to agents: an issue
tracker, a docs search, your mail. The daemon is the MCP client: it signs in,
keeps the tokens in the keychain and passes each tool call on. Tokens never
enter a sandbox, and an agent only sees the connectors you give it.

## Add a connector

Open **Settings → Connectors**.

- **Catalog**: popular MCP servers run by their vendors. Click **Add** on a
  card. Each card says how it signs in: **Sign in** (OAuth), **Token** or
  **No sign-in**.
- **Add connector**: any other server. Give it a name (lowercase letters,
  digits and dashes, at most 16 characters) and its URL. Agents see its tools
  as `name__tool`.

## Token or OAuth

- **Token connectors**: adding one from the catalog opens **Vault**; for one
  you added by hand, open Vault yourself. Paste the token there; it goes to
  the daemon's keychain and is sent as a Bearer token.
- **OAuth connectors**: adding one from the catalog opens the provider's
  sign-in page in your browser; otherwise click the sign-in button on its row
  (**Sign in to name**). Sign in and allow access. The page then says "Signed in. You can
  close this tab and go back to ferrobot." The row in Settings turns to
  **connected** by itself. A sign-in waits up to 10 minutes for you.
- Nothing opened? The row shows **Open the sign-in page**. A connector that
  needs it later shows a sign-in button again (for example "sign-in expired,
  sign in again"). The sign-out button signs it out and revokes the token
  where the server allows it.

## The connection check

When you add a connector, and after a sign-in, the daemon connects to it as
an MCP client and lists its tools. The row shows the result:

- **connected · 12 tools**: it works.
- **needs sign-in**: the server wants credentials. Sign in, or add its token
  in Vault.
- **failed: ...**: unreachable, not an MCP server, or it refused the token.
  The reason is ferrobot's own text, never the server's, because a server's
  error could echo the token.
- **not checked yet**: after a daemon restart; opening Settings checks it
  again.

## Give it to an agent

Open the agent's **Edit agent** dialog. Its **Tools** list has a switch per
connector. Changes apply from the agent's next turn.

- An agent can ask for a connector with its `add_connector` tool. You see a
  card with the name and URL and, for a token, a hidden field. The token goes
  straight to Vault; the agent never sees it.
- An agent that creates another agent can give it at most its own
  connectors. See [[Tools and permissions|Tools-and-Permissions]].

## Own OAuth app

Most catalog servers register ferrobot as an OAuth client by themselves
(dynamic client registration). Some providers do not, for example Google,
GitHub and Slack. For those you create an OAuth app at the provider and give
ferrobot its details:

- **OAuth client ID**: from the provider.
- **Issuer**: the provider's sign-in server. Sign-ins through any other server
  are refused.
- **Scopes**: optional, space separated.
- **Redirect URI**: add `http://127.0.0.1:7878/v1/oauth/callback` at the
  provider. The form shows the exact value for your daemon.
- **Client secret**: goes in **Vault**, never in the form.

For a new connector, tick **Use my own OAuth app** in the **Add connector**
form. For one you already added, click its gear button (**OAuth app of
name**), fill in the fields and click **Save OAuth app**. Changing the client
ID or issuer signs the connector out and removes its client secret from Vault,
so add the secret after saving.

## Example: Gmail

Gmail's official MCP server is `https://gmailmcp.googleapis.com/mcp/v1`. It is
in Developer Preview: your Google Workspace account must be in the Google
Workspace Developer Preview Program. Its tools search and read mail, manage
labels and write drafts. It cannot send mail.

In the Google Cloud console:

1. Pick or create a Google Cloud project.
2. Enable the **Gmail API** (`gmail.googleapis.com`) and the **Gmail MCP
   API** (`gmailmcp.googleapis.com`).
3. Set up the consent screen under **Google Auth Platform → Branding** and
   **Audience**. Choose **Internal**, or **External** and add yourself as a
   test user.
4. Under **Data Access**, add these scopes:
   - `https://www.googleapis.com/auth/gmail.readonly`
   - `https://www.googleapis.com/auth/gmail.compose`
   - `https://www.googleapis.com/auth/gmail.modify`
5. Create an OAuth client of type **Web application**. Add the redirect URI
   `http://127.0.0.1:7878/v1/oauth/callback` and tick **Use this client for an
   AI-powered agent**.
6. Copy the client ID and the client secret.

In ferrobot:

1. In **Settings → Connectors**, add **Gmail** from the catalog. Adding it
   starts a sign-in right away, which cannot work yet: close that browser tab.
   Without a catalog card, use **Add connector** with the name `gmail` and
   the URL above.
2. Click the gear button (**OAuth app of gmail**) and enter:
   - OAuth client ID: your client ID
   - Issuer: `https://accounts.google.com`
   - Scopes: `https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.compose https://www.googleapis.com/auth/gmail.modify`
3. Click **Save OAuth app**.
4. Open **Vault**. Under Connectors, add the **OAuth client secret** of gmail.
5. Back in **Settings → Connectors**, click **Sign in to gmail** (the sign-in
   button on its row), sign in with Google and allow access. The row turns
   **connected**.
6. Give `gmail` to an agent in its **Edit agent** dialog.

## What has been tested

Connector tokens, OAuth and the approval card run in the daemon and are
covered by its tests against fake servers. Not every catalog server has been
tried with a real account, and the Gmail steps follow Google's Developer
Preview, which may change.
