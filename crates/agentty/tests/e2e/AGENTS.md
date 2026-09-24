# Agentty PTY Scenarios

- Build visible UI scenarios with `FeatureTest` in `crates/agentty/tests/e2e/common.rs`.
  Reuse shared journeys and assertions; assert observable terminal behavior and keep
  volatile output deterministic.
- Use `.zola(...)` only when publishing a feature demo. Keep its `snake_case` test name,
  page, GIF, hash sidecar, and PNG poster aligned.
- Follow `docs/contributing/feature-test/authoring.md` for focused scenario validation
  and `docs/contributing/feature-test/recording.md` for published artifacts.
