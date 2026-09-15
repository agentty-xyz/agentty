# agentty

Main Ratatui application for managing agent sessions.

## Boundaries

- Keep application-specific database location and clock composition here; reusable
  repositories, SQL, and migrations belong in `ag-store`.
- Follow the nearest guide under `crates/agentty/src/` for layer-specific rules.

## Integration

- Compose reusable crate adapters at the application boundary and inject their public
  traits into workflows. Keep product-specific permissions and UI policy here.
- Expose programmatic lifecycle operations through the host `SessionBackend` adapter;
  background consumers use `SessionService` and channels rather than mutating UI state.

## Documentation

Use `docs/site/content/docs/architecture/module-map.md` to route changes to the owning
layer and `docs/site/content/docs/architecture/runtime-flow.md` for composition and
channel wiring. Keep `README.md` and `docs/site/content/docs/usage/` aligned with public
application behavior.
