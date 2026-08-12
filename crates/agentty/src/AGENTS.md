# Source Code

## Local Conventions

- Avoid near-identical local variable names in the same function (for example, `gitdir`
  and `git_dir`). Use one clear naming style with distinct, descriptive names.
- Persisted keys in `setting` and `project_setting` must come from
  `ag_session::SettingName`, re-exported by `domain/setting.rs`; do not introduce ad hoc
  string keys or legacy aliases.
- Prefer `module.rs` plus `module/` for nested modules. Avoid `mod.rs` module roots.
- Session status flow:
  - `SessionStatus::can_transition_to()` in `crates/ag-session/src/model.rs` is the
    canonical transition graph. Read and update that function and its tests instead of
    duplicating the full state machine in prose.
  - `Draft` is set when `create_session()` creates a blank session before the user types
    a prompt.
  - `InProgress` can be entered from `Draft` (first prompt) or from `Review`/`Question`
    (reply).
  - `Question` is set when a completed turn returns structured clarification questions.
  - When agent response finishes, all changes are auto-committed and status is set to
    `Review` or `Question`.
  - Review request sync records an upstream merge as read-only `Merged`; only a
    successful manual target-branch sync advances it to `Done` and starts cleanup.
  - `Done` can also be entered after local merge cleanup succeeds.
  - While agent is preparing a response, status is `InProgress`.

## Docs Sync

When changing architecture-level behavior under `src/`, update:

- `docs/site/content/docs/architecture/module-map.md` — module/path ownership and
  boundaries.
- `docs/site/content/docs/architecture/runtime-flow.md` — runtime flow, channel
  transport, and turn interaction flow.
- `docs/site/content/docs/architecture/testability-boundaries.md` — trait boundaries for
  external integrations.
- `docs/site/content/docs/architecture/change-recipes.md` — contributor-safe change
  paths.
- Keep the runtime-mode file list in `module-map.md` aligned with actual files under
  `runtime/mode/`.
- Keep the key-type tables/field descriptions in `runtime-flow.md` aligned with
  `crates/ag-agent/src/channel/contract.rs` (re-exported by
  `crates/ag-agent/src/channel.rs`) for `TurnRequest`, `TurnContinuation`, `TurnEvent`,
  and `TurnResult`.
- Keep `testability-boundaries.md` aligned with active
  `#[cfg_attr(test, mockall::automock)]` trait boundaries that guard
  external/time/process integrations.

## Major Areas

- `app.rs` and `app/` own orchestration and workflow state.
- `domain.rs` and `domain/` own Agentty-specific business entities and compatibility
  re-exports for shared models from `ag-session`.
- `infra.rs` and `infra/` own external integrations and persistence.
- `runtime.rs` and `runtime/` own terminal lifecycle and event dispatch.
- `ui.rs` and `ui/` own rendering, layout, and interaction widgets.
