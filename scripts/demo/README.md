# Demo recording

`record.sh <scene>` records one scene from the Vite UI in Chromium and encodes it, so the videos
can be re-recorded after UI changes:

```
DEMO_API=http://127.0.0.1:17990 DEMO_TOKEN_FILE=<data dir>/api-token scripts/demo/record.sh hero
```

- **Keyed takes** use the daemon `DEMO_API` points at and its provider named `anthropic`
  (`DEMO_PROVIDER`). Use a throwaway daemon: the scenes create and delete agents named
  `researcher` and `builder`, add the DeepWiki connector from the catalog, and add `gcc` and
  `libc6-dev` under CLI packages, because `warm-rust.sh` needs a linker for builder's Rust.
- **Keyless dry runs**: without `DEMO_API`, it starts its own daemon with the mock driver on 17992.
  The story then stops at the first model reply; use it to check the clicks and the encode.
- **Scenes:** `hero` (dark mode) writes `docs/assets/demo.{mp4,gif}` and the X and LinkedIn cuts in
  `docs/launch/media/`; `keycard`, `routine`, `memory` and `usage` write the clips in
  `docs/assets/clips/`; `shots` writes light and dark screenshots to `docs/screenshots/`. Record
  `hero` first, because the others reuse its agents, and `usage` and `shots` last.
- **Re-encoding:** with `DEMO_KEEP=1` the frames dir stays, and
  `node scripts/demo/record.mjs encode <scene> <frames dir>` encodes it again (for example after
  tuning a cut in `CUTS`).

Waits for the model run at 2x under a visible "2x" badge; the middle of a long wait is cut.
Off-camera setup (roles, warming builder's toolchain) is cut out. The cursor, zooms and captions
are injected by `lib.mjs` and never ship in the app. Needs Docker, `apps/desktop/node_modules`,
the global Playwright from `@playwright/cli` with a cached Chromium, and ffmpeg.
