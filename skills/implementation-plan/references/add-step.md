# Add Step

Use this guide when roadmap work is missing entirely and needs a new backlog item.

## Goal

Insert one new roadmap item into the correct queue in `docs/plan/roadmap.md` using a stable UUID in the `[UUID] Stream: Title` heading.

## Workflow

1. Read `docs/plan/roadmap.md`, the current streams, and the `Ready Now` execution order before adding anything.
1. Confirm the work is a new atomic acceptance story instead of a revision to an existing item.
1. Decide whether the work belongs in `## Ready Now`, `## Queued Next`, or `## Parked`.
1. For `Ready Now`, prepare one stream name, one step title, one `#### Why now` sentence, one `#### Usable outcome` sentence, and the concrete `#### Substeps`, `#### Tests`, and `#### Docs` bullets for that slice.
1. For `Queued Next` or `Parked`, prepare one stream name, one step title, one `#### Outcome` sentence, one `#### Promote when` sentence, and one `#### Depends on` value.
1. Insert the new item using the canonical layout from `skills/implementation-plan/SKILL.md`, give it a fresh UUID in the `[UUID] Stream: Title` heading, and place it where the execution order or promotion queue should reflect the new work.
1. Re-read the inserted item and then manually reconcile any roadmap sections outside that queue that the new work affects.

## Guardrails

- Adding a `Ready Now` step usually also requires manual updates to `## Active Streams`, `## Planning Model`, `## Ready Now Execution Order`, or `## Context Notes` when the new work changes roadmap flow.
- Keep new `Ready Now` steps at `XL` or smaller and split them before insertion if they would exceed the skill's size budget.
- Prefer `No assignee` for new `Ready Now` steps unless the user explicitly wants to claim the work immediately.
- Prefer adding backlog work to `## Queued Next` or `## Parked` instead of expanding `## Ready Now` beyond `5` items.
