# Runtime Layer

Owns the foreground terminal lifecycle, input polling, and mode dispatch.

- Keep business workflows in `app` and frame rendering in `ui`.
- Dispatch user intent to app/infra boundaries.
- When key handling changes, keep rendered help actions and
  `docs/site/content/docs/usage/keybindings.md` aligned.
