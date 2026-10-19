+++
title = "Task"
description = "Desired outcomes and explicit attempts through session workers."
weight = 6
+++

A Task records a desired outcome in one
[session](@/docs/core-components/durable-components/session.md). The session supplies
conversation and workspace context. Each task attempt runs one turn and its ordered
post-processing through a unique
[operation](@/docs/core-components/durable-components/operation.md).

## Planning and Querying

Use `SessionService::create_task` with a `CreateTaskRequest` to save a nonempty
`task_id` and `objective` before execution. The owning session must exist in storage,
but may be draft, busy, or terminal. Creation adds no attempt, operation, message, or
worker work and does not change the session. Repeating the same request preserves the
task, including its creation time and completion; changing its session or objective is
rejected.

Use `SessionService::list_tasks` to read all tasks belonging to one session, including
unattempted and completed tasks with their attempt histories. Results form one storage
snapshot in creation order, with insertion order breaking timestamp ties. A missing
session or a session without tasks returns an empty list. Use `SessionService::get_task`
to retrieve one task by ID.

One session can own multiple distinct tasks. Their attempts execute serially through
that session's worker; submitting a new attempt while the session is busy is rejected,
not added to the ordinary chat queue. Tasks in different eligible sessions can execute
concurrently within existing capacity and workspace limits. Each task stays with its
owning session. Task management is programmatic; there is no dedicated task UI or
dependency scheduler.

## Built-in Agent Definitions

`ag_session::BuiltinAgentDefinition` provides two role contracts for future task
workflows:

| Role          | Required context                               | Permissions                                     | Expected answer                        |
| ------------- | ---------------------------------------------- | ----------------------------------------------- | -------------------------------------- |
| `Planner`     | User request and clarification                 | Read only                                       | Proposed plan and validation steps     |
| `Implementer` | User request, accepted plan, and clarification | Session permissions, including read-only limits | Observed changes, checks, and blockers |

Each definition supplies role instructions and a permission resolver. Roles are separate
from providers and personalities and do not select a model; a workflow can supply the
same session Smart `AgentSelection` to both. Questions use the existing response
protocol, and neither output automatically completes a task.

These are library contracts only. Task submission does not yet assign roles or apply
their instructions. A workflow must provide separate conversations, explicit accepted
plan handoff, and worker-enforced permissions before running a chain. Definitions do not
add a registry, editor, persistence, or automatic scheduling.

## Submission and Completion

Programmatic callers use `SessionService::submit_task` with a `SubmitTaskRequest`.
Choose a stable `task_id`, an immutable `objective`, and a globally unique `attempt_id`.
The `message` instructs that attempt; `visibility` controls its transcript presentation.
New attempts require a loaded session in `Review`, `AgentReview`, or `Question`, even
when the task was planned earlier. Session creation and ordinary chat keep their
existing behavior.

Submission creates the task if needed, preserving existing `submit_task` behavior.
Admission atomically saves the task, attempt, and queued operation before worker
delivery; a pre-created task retains its identity and creation time. A successful
submission acknowledges admission, not execution. Redelivering identical inputs with the
same attempt ID acknowledges the existing attempt, including after completion or
failure. Changed inputs under an existing identity are rejected. An enqueue failure can
leave a failed attempt; inspect it before retrying.

Attempts project queued, running, succeeded, failed, or canceled state from their
operations. Running includes ordered post-processing. A successful turn does not
establish that the objective is satisfied: only `SessionService::complete_task` records
the caller's explicit acceptance. Completion requires all attempts to have settled and
prevents new attempts. A task with zero attempts can also be explicitly accepted.
Completion does not merge, publish, or finish the session.

## Clarification, Retries, and Recovery

A clarification response finishes its attempt and leaves the task open and the session
waiting for input. Read the session's questions and submit a new attempt with the
answers in its message to continue task accounting. Ordinary question answers retain
their existing session behavior and are not automatically attached to a task.

Use a new attempt ID for an explicit retry or continuation. Provider retries remain
inside the current attempt. Only one attempt per task may be unfinished; dependency
scheduling and automatic retries are not provided.

Session cancellation settles its attempts through ordinary operation cancellation and
leaves the desired outcome open. Restart recovery marks unfinished operations and their
attempts failed without replaying them. Earlier attempts retain their identities and
history. Deleting the owning session deletes its tasks and attempts. Utility
[runs](@/docs/core-components/durable-components/run.md) remain separate records.
