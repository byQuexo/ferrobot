## Summary

<!-- What changes and why, in a few lines. Link the issue if there is one. -->

**Not tested:** <!-- what you did not run or could not check, e.g. "Docker e2e", "Tauri window", "real provider" -->

## Checklist

- [ ] The title is a Conventional Commit (`feat:`, `fix:`, `refactor:`, `test:`, `docs:`, `chore:`), at most 72 characters.
- [ ] The gates from [CLAUDE.md "Commands"](../blob/main/CLAUDE.md#commands) for the parts I touched pass locally.
- [ ] No provider key or operator token reaches a sandbox, its env, a log, an event or an API response.
- [ ] New behaviour has a test.
- [ ] If the API changed: `protocol/` is regenerated (`FERROBOT_UPDATE_SPECS=1 cargo test -p ferrobot-daemon --test specs`) and so are the desktop types (`npm run gen:api`).
- [ ] If a roadmap phase changed: `docs/progress.md` is updated.
- [ ] Comments explain only the why (see [docs/CONVENTIONS.md](../blob/main/docs/CONVENTIONS.md#comments)).
