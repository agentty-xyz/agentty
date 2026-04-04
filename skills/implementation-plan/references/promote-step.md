# Promote Step

Use this guide when a compact `Queued Next` or `Parked` card is being promoted into `## Ready Now`.

## Goal

Promote one backlog card into `Ready Now`, expand it into the full ready-step shape, and assign ownership in that same roadmap edit so implementation can begin without a separate claim-only step.

## Workflow

1. Read `docs/plan/roadmap.md` and find the target card by the UUID in its `[UUID] Stream: Title` heading.
1. Verify the target lives in `## Queued Next` or `## Parked`. If it is already in `## Ready Now`, use `references/update-step.md` instead of re-promoting it.
1. Run `cargo run -q -p ag-xtask -- roadmap context-digest` before reshaping the roadmap so the promotion uses fresh repository context.
1. Run `gh api user --jq .login` and use that login as the promotion default.
1. Choose the `#### Assignee` value during promotion. Use an explicit `@username` when the user or team has already named the owner; otherwise default to the authenticated promoter's `@<login>`.
1. Remove the compact card from its source queue, insert the expanded `Ready Now` step in the active execution window, and render `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs` in the canonical ready-step layout.
1. Re-read the updated roadmap and verify the promoted step is present only once, has a concrete `@username` assignee, and any affected execution diagram, stream list, or context notes were reconciled.

## Guardrails

- Do not leave a promoted `Ready Now` step with `No assignee`.
- Do not add assignees to `## Queued Next` or `## Parked`; ownership lives only in `## Ready Now`.
- Do not skip the queue-to-ready rewrite by pasting ready-step subsections into the old compact card in place.
- Stop and clarify if the promoted card depends on work that is still missing or if the intended assignee is ambiguous and should not default to the promoter.
