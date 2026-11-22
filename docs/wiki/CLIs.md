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
`imagemagick`, `ffmpeg`, `graphviz`, `zip`, `unzip`, `tree`, `python3-pip`
and `python3-venv`.

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
- Only keyless tools. Tools that need a login (`gh`, `aws`, `gcloud`) are left
  out on purpose: keys never enter a sandbox. Use a connector for those
  services instead. See [[Connectors and OAuth|Connectors-and-OAuth]].
- One build at a time. A second **Build image** while one runs is refused.
- A build that takes longer than 15 minutes fails.

## Without the Shell tool

The **Shell** tool group is what runs commands, so an agent without it cannot
use any CLI. See [[Tools and permissions|Tools-and-Permissions]].
