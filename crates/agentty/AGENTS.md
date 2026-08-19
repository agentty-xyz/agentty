# agentty

Main Ratatui application for managing agent sessions.

- Keep application-specific database location and clock composition here; reusable
  repositories, SQL, and migrations belong in `ag-store`.
- Use `docs/site/content/docs/architecture/module-map.md` for path ownership,
  `docs/site/content/docs/architecture/runtime-flow.md` for orchestration and channels,
  and `docs/site/content/docs/architecture/testability-boundaries.md` for external
  boundaries.
- Follow the nearest guide under `src/` for layer-specific rules.
