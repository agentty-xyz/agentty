# ag-orchestration

Frontend-neutral campaign planning, approval, reconciliation, and integration policy.

## Boundaries

- Own campaign sequencing and controller/child prompts here; keep terminal state and
  rendering in the host.
- Read durable campaign state through repositories and perform session mutations through
  `SessionService`. Keep Git mechanics behind `GitClient`.
- Persist validated plans before approval and keep reconciliation recoverable from
  stored state.

## Integration

- Hosts supply session capabilities, repositories, Git, `OrchestrationEventSink`, and
  `OrchestrationSchedule`. Notification delivery must not wait for frontend processing.
- Use durable coordinator submissions for controller turns instead of the ordinary
  live-chat queue. Preserve stable operation identifiers when retrying submissions.
- Keep templates under `crates/ag-orchestration/src/template/` synchronized with their
  renderers and shared protocol models.

## Documentation

Follow `docs/site/content/docs/architecture/change-recipes.md` for campaign changes.
Update `docs/site/content/docs/architecture/orchestrator.md` for campaign design and
`docs/site/content/docs/usage/workflow.md` for approval or integration behavior.
