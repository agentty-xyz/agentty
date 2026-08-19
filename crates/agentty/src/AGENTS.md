# Agentty Source

`docs/site/content/docs/architecture/module-map.md` is the canonical layer and ownership
map. Nested guides specialize the rules for `app`, `domain`, `infra`, `runtime`, and
`ui`.

## Invariants

- Persisted `setting` and `project_setting` keys come from `ag_session::SettingName`,
  re-exported by `domain/setting.rs`; do not add ad hoc keys or aliases.
- `ag_session::SessionStatus::can_transition_to()` is the lifecycle source of truth. Do
  not restate or implement a second transition graph in Agentty.

## Architecture Sync

- Update `docs/site/content/docs/architecture/module-map.md` for ownership changes,
  `docs/site/content/docs/architecture/runtime-flow.md` for orchestration or channel
  changes, `docs/site/content/docs/architecture/testability-boundaries.md` for external
  traits, and `docs/site/content/docs/architecture/change-recipes.md` for contributor
  change paths.
- Keep those pages at the architecture level; source routers and doc comments own
  file-level detail.
