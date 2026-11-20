# Demo recording

`record.sh` re-records `docs/assets/demo.mp4` and `demo.gif` against a throwaway daemon with a real
model, so run it again after UI changes. Pipe the provider's API key on stdin:

```
<print your OpenRouter key> | scripts/demo/record.sh
```

It needs Docker with `ferrobot-runner:dev`, `apps/desktop/node_modules` (`npm ci`), the global
Playwright from `@playwright/cli` with a cached Chromium, and ffmpeg. The cursor and zooms are injected
by `record.mjs` and never ship in the app; the lead's role in `record.mjs` is the story.
