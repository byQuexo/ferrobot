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
`python3-venv` and `gh` (with the GitHub login below).

## GitHub login (gh and git push)

Agents can use `gh` and push with `git` over HTTPS without the token ever
entering their computer.

1. Create a token on GitHub (a fine-grained one, limited to the repositories
   and permissions the agents need) and paste it in **Settings → Vault → CLI
   logins → GitHub**.
2. In **Settings → CLIs → GitHub login**, pick the agents that may use it.
   Their computers restart once; their files stay.
3. Add `gh` to the tools and build the image if the agents should use the
   GitHub CLI; `git` is already there.

How it works: the picked agents get a stand-in token (`GH_TOKEN`, and git's
credential helper) and send HTTPS through the daemon (`HTTPS_PROXY`, with the
agent's own token as the password). For `github.com`, `api.github.com` and
`uploads.github.com` the daemon ends TLS with a certificate from ferrobot's
local CA, which their computers trust and which can only vouch for
`github.com` names. There it swaps the stand-in for the real token and
forwards the request, without following redirects. Every other public host
gets a plain tunnel; private addresses (this computer, your network) are
refused. The CA's key and the token stay in the daemon's keychain.

Merging a pull request asks first; deleting a repository, publishing a
release and force-pushing ask even in Auto mode. Tools that sign every request
with the key (AWS, Google Cloud) or pin their certificates cannot work this
way.

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
- Only keyless tools, apart from the GitHub login above. Other tools that need
  a login (`aws`, `gcloud`) are left out on purpose: keys never enter a
  sandbox. Use a connector for those services instead. See
  [[Connectors and OAuth|Connectors-and-OAuth]].
- One build at a time. A second **Build image** while one runs is refused.
- A build that takes longer than 15 minutes fails.

## Without the Shell tool

The **Shell** tool group is what runs commands, so an agent without it cannot
use any CLI. See [[Tools and permissions|Tools-and-Permissions]].
