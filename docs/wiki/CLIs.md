CLIs are command-line tools in every agent's computer: `jq`, `pandoc`,
`ffmpeg` and the like. They are Debian packages, built into one shared
sandbox image on top of the runner image (Debian bookworm). Every agent with
the **Shell** tool can use them.

## Add CLIs

1. Open **Settings → CLIs**.
2. Pick tools from the list, or type another Debian package name under
   **Other Debian package** and click **Add**.
3. Click **Build image**. The build runs in the background and can take a few
   minutes.

The list offers: `jq`, `sqlite3`, `poppler-utils` (PDF text), `pandoc`,
`imagemagick`, `ffmpeg`, `graphviz`, `zip`, `unzip`, `tree`, `python3-pip`,
`python3-venv` and `gh` (with a GitHub login below).

## Logins (gh, git push and other CLIs with a token)

Agents can use CLIs that read a token from an environment variable, like `gh`,
without the token ever entering their computer.

1. In **Settings → CLIs → Logins**, choose **Add login**. **GitHub** is ready
   to use; for another CLI enter a name, its API hosts (e.g.
   `api.example.com`), the variables it reads its token from (e.g.
   `EXAMPLE_API_KEY`) and the programs that use it.
2. Paste the token in **Settings → Vault → CLI logins**. For GitHub, use a
   fine-grained token limited to the repositories and permissions the agents
   need.
3. Pick the agents that may use the login. Their computers restart once; their
   files stay.
4. Add the CLI to the tools and build the image if it is a Debian package
   (`gh` is in the list; `git` is already there).

How it works: the picked agents get a stand-in token in the login's variables
(for GitHub also git's credential helper) and send HTTPS through the daemon
(`HTTPS_PROXY`, with the agent's own token as the password). For the login's
hosts the daemon ends TLS with a certificate from that login's own local CA,
which their computers trust and which can only vouch for those hosts. There it
swaps the stand-in for the real token in the request headers (never in the
address), puts the stand-in back if a response header repeats the token, and
forwards the request to a public address, without following redirects. Every
other public host gets a plain tunnel; private addresses (this computer, your
network) are refused. The CAs' keys and the tokens stay in the daemon's
keychain.

A login's hosts must be public names (no IP addresses, `localhost` or
`.local`/`.internal` names), and its variables cannot be ones the computer
needs itself (the proxy and certificate settings, `PATH`, `FERROBOT_…`).

**Ask before every command that uses it** (on by default for a new login, off
for GitHub) puts a card before each command that runs one of the login's
programs or names one of its variables or hosts, unless Auto mode is on. It
looks at the command line only: a script the agent wrote can still reach the
login. For GitHub, merging a pull request asks first; deleting a repository,
publishing a release and force-pushing ask even in Auto mode.

It cannot work for CLIs that ignore `HTTPS_PROXY` or the system's certificate
settings, that sign every request with the key (AWS, Google Cloud), that pin
their certificates, or whose tokens refresh themselves.

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
- At most 32 packages. Names follow Debian's rules: lowercase letters,
  digits, `+`, `.` and `-`.
- Only keyless tools, apart from the logins above. Tools that sign each request
  with a key (`aws`, `gcloud`) are left out on purpose: keys never enter a
  sandbox. Use a connector for those services instead. See
  [[Connectors and OAuth|Connectors-and-OAuth]].
- One build at a time. A second **Build image** while one runs is refused.
- A build that takes longer than 15 minutes fails.

## Without the Shell tool

The **Shell** tool group is what runs commands, so an agent without it cannot
use any CLI. See [[Tools and permissions|Tools-and-Permissions]].
