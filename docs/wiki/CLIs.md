CLIs are command-line tools in the agents' computers: `jq`, `pandoc`, `gh`,
an npm tool, a program from a download link. Every agent with the **Shell**
tool can use the ones it has.

## Add a CLI

1. Open **Settings → Computers** and choose **Add CLI**.
2. Pick a known CLI, or give it a name and choose how it installs:
   - **Debian package**: built into the one image every computer starts from
     (Debian bookworm). Every agent gets it; the build takes a few minutes and
     agents switch when their computer next starts or wakes.
   - **npm package**, **Download link** (the program itself; add its SHA-256
     to have it checked) or **Install script** (runs as the agent's user and
     should put the CLI in `~/.local/bin`): installed into the home folder of
     each agent you pick, once, right away for running agents and on the next
     wake for the others. Nobody else rebuilds or restarts, and recreating a
     computer keeps it.
3. List its **Programs** (needed for a download, and for "ask first").
4. Pick the agents and save.

The known list has `jq`, `sqlite3`, `poppler-utils` (PDF text), `pandoc`,
`imagemagick`, `ffmpeg`, `graphviz`, `zip`, `unzip`, `tree`, `python3-pip`,
`python3-venv` and `gh` (with its account and sign-in filled in). Agents can
also install tools that need no token into `~/.local` themselves.

## Accounts (gh, git push and other CLIs with a token)

A CLI that reads a token from an environment variable can have an account;
the token never enters an agent's computer.

- **API key**: paste it in the dialog (or later in **Settings → Vault**). For
  GitHub, use a fine-grained token limited to the repositories and
  permissions the agents need.
- **Sign in with the CLI**: for CLIs whose own sign-in shows a one-time code
  (`gh auth login --web`). The daemon runs it in a throwaway computer from the
  agents' image (no agent token, none of your files), shows you the address
  and code, then reads the token with the CLI's token command (`gh auth
  token`), puts it in the vault and removes that computer. Cancel removes it
  too. The CLI sees your token once during its own sign-in, exactly like
  running it on your computer; for a CLI you do not trust, paste a narrowly
  scoped API key instead. CLIs that wait for the browser to call back to
  `localhost` are not supported; use an API key for those.
- **Advanced**: the API hosts that get the token, the variables the CLI reads
  it from, and for a sign-in its login and token commands.

How it works: the picked agents get a stand-in token in the account's
variables (for a CLI with `github.com` also git's credential helper) and send
HTTPS through the daemon (`HTTPS_PROXY`, with the agent's own token as the
password). For the account's hosts the daemon ends TLS with a certificate
from that CLI's own local CA, which their computers trust and which can only
vouch for those hosts. There it swaps the stand-in for the real token in the
request headers (never in the address), puts the stand-in back if a response
header repeats the token, and forwards the request to a public address,
without following redirects. Every other public host gets a plain tunnel;
private addresses (this computer, your network) are refused.

An account's hosts must be public names (no IP addresses, `localhost` or
`.local`/`.internal` names), and its variables cannot be ones the computer
needs itself (the proxy and certificate settings, `PATH`, `FERROBOT_…`).

**Ask before every command that uses it** (on by default, off for `gh`) puts
a card before each command that runs one of the CLI's programs or names one
of its variables or hosts, unless Auto mode is on. It looks at the command
line only: a script the agent wrote can still reach the account. For `gh`,
merging a pull request asks first; deleting a repository, publishing a
release and force-pushing ask even in Auto mode.

It cannot work for CLIs that ignore `HTTPS_PROXY` or the system's certificate
settings, that sign every request with the key (AWS, Google Cloud), that pin
their certificates, or whose tokens refresh themselves.

## Agents propose CLIs

An agent with the **Add connectors** tool can ask for a CLI (`propose_cli`).
Your card shows the exact install (package, address with or without a
checksum, or the whole script) and the hosts its token would go to; Auto mode
never skips it. Approving adds the CLI for that agent; an account still needs
your key or sign-in.

## Desktop

The **Desktop** switch adds a screen to every agent's computer: a dark desktop
with a dock, Chromium, a terminal and a file manager (about 170 MB more
image). It starts when it is first needed and runs until the computer
stops.

- The computer card at the top of the **Computer** tab shows a small live
  view; click it to open the screen over the whole window (**Close** or Escape
  leaves). It is view only until you press **Use it myself**; **Hand back**
  returns it. While you use it, the agent's Desktop tool tells it to wait.
- Agents use it with the **Desktop** tool group, which you switch on per agent
  (see [[Tools and Permissions|Tools-and-Permissions]]): screenshots, clicks,
  typing, keys and scrolling, run by the daemon in the agent's computer.
- The view travels through the daemon; nothing in the computer listens on a
  port.
- An agent with a CLI account cannot have the Desktop tool, and the other way
  round: commands typed into the desktop's terminal skip the checks on risky
  steps.

## When agents get them

- New agents start from the new image.
- An agent that already has a computer switches when it next starts or wakes:
  the daemon sees the outdated image and recreates the computer. Its home
  directory is kept.
- If a build fails, agents keep the previous image and the page shows why.
- Removing every CLI and building again goes back to the plain runner image.
- A daemon restart rebuilds the image, usually from Docker's cache.

## Limits

- Docker only. On other backends the page says "CLIs need the Docker
  backend".
- At most 32 Debian packages. Names follow Debian's rules: lowercase
  letters, digits, `+`, `.` and `-`.
- Tokens only through accounts. Tools that sign each request
  with a key (`aws`, `gcloud`) are left out on purpose: keys never enter a
  sandbox. Use a connector for those services instead. See
  [[Connectors and OAuth|Connectors-and-OAuth]].
- One build at a time. A second **Build image** while one runs is refused.
- A build that takes longer than 15 minutes fails.

## Without the Shell tool

The **Shell** tool group is what runs commands, so an agent without it cannot
use any CLI. See [[Tools and permissions|Tools-and-Permissions]].
