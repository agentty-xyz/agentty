# Promote Step

Use this guide when a compact `Queued Next` or `Parked` card is being promoted into `## Ready Now`.

## Goal

Promote one backlog card into `Ready Now`, expand it into the full ready-step shape, and assign ownership in that same roadmap edit so implementation can begin without a separate claim-only step.

## Workflow

1. Read `docs/plan/roadmap.md` and find the target card by the UUID in its `[UUID] Stream: Title` heading.
1. Verify the target lives in `## Queued Next` or `## Parked`. If it is already in `## Ready Now`, use `references/update-step.md` instead of re-promoting it.
1. Run `cargo run -q -p ag-xtask -- roadmap context-digest` before reshaping the roadmap so the promotion uses fresh repository context.
1. Choose the `#### Assignee` value during promotion. Use an explicit `@username` when the user or team has already named the owner; otherwise resolve the current authenticated forge user for the active project and use that `@username`.
1. Check the active `Ready Now` window before inserting the step. For agent-backed two- or three-person development, keep `Ready Now` at `2..=5` active implementation items.
1. Remove the compact card from its source queue, insert the expanded `Ready Now` step in the active execution window, and render `#### Assignee`, `#### Why now`, `#### Usable outcome`, `#### Substeps`, `#### Tests`, and `#### Docs` in the canonical ready-step layout.
1. Re-read the updated roadmap and verify the promoted step is present only once, has a concrete `@username` assignee, and any affected execution diagram, stream list, or context notes were reconciled.

## Guardrails

- Do not leave a promoted `Ready Now` step without a concrete `@username` assignee.
- Do not add assignees to `## Queued Next` or `## Parked`; ownership lives only in `## Ready Now`.
- Do not skip the queue-to-ready rewrite by pasting ready-step subsections into the old compact card in place.
- Do not exceed the normal `Ready Now` capacity just because space exists in the file.
- Stop and clarify if the promoted card depends on work that is still missing or if the intended assignee is ambiguous and should not default to the promoter.
