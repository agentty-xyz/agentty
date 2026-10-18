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

## Submission and Completion

Programmatic callers use `SessionService::submit_task` with a `SubmitTaskRequest`.
Choose a stable `task_id`, an immutable `objective`, and a globally unique `attempt_id`.
The `message` instructs that attempt; `visibility` controls its transcript presentation.
New attempts require a loaded session in `Review`, `AgentReview`, or `Question`.
Creation and ordinary chat keep their existing behavior.

Admission atomically saves the task, attempt, and queued operation before worker
delivery. A successful submission acknowledges admission, not execution. Redelivering
identical inputs with the same attempt ID acknowledges the existing attempt, including
after completion or failure. Changed inputs under an existing identity are rejected. An
enqueue failure can leave a failed attempt; inspect it before retrying.

Use `SessionService::get_task` to read the objective and attempt history. Attempts
project queued, running, succeeded, failed, or canceled state from their operations.
Running includes ordered post-processing. A successful turn does not establish that the
objective is satisfied: only `SessionService::complete_task` records the caller's
explicit acceptance. Completion requires all attempts to have settled and prevents new
attempts. It does not merge, publish, or finish the session.

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
