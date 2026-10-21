+++
title = "Task"
description = "Desired outcomes and explicit attempts through session workers."
weight = 6
+++

A Task records a desired outcome in one
[session](@/docs/core-components/durable-components/session.md). The session owns its
workspace and scheduling. Ordinary tasks use session conversation context; role-aware
tasks retain their own conversation. Each attempt runs one turn and its ordered
post-processing through a unique
[operation](@/docs/core-components/durable-components/operation.md).

`Task::env_id()` identifies the session-owned
[execution environment](@/docs/core-components/durable-components/session.md#execution-environment)
shared by all its attempts. Task completion does not release that workspace.

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
owning session. Generic task management is programmatic. The
[Plan then implement workflow](@/docs/usage/workflow.md#plan-then-implement) drives its
two role tasks from the session chat; arbitrary dependency scheduling is not provided.

## Built-in Agent Definitions

`ag_session::BuiltinAgentDefinition` provides two built-in task roles:

| Role          | Required context                               | Permissions                                     | Expected answer                        |
| ------------- | ---------------------------------------------- | ----------------------------------------------- | -------------------------------------- |
| `Planner`     | User request and clarification                 | Read only                                       | Proposed plan and validation steps     |
| `Implementer` | User request, accepted plan, and clarification | Session permissions, including read-only limits | Observed changes, checks, and blockers |

Each definition supplies role instructions and a permission resolver. Roles are separate
from providers and personalities and do not select a model; a workflow can supply the
same session Smart `AgentSelection` to both. Questions use the existing response
protocol, and neither output automatically completes a task.

`SubmitAgentTaskRequest` describes an isolated role-aware attempt separately from
ordinary `SubmitTaskRequest` execution. It carries a captured provider/model selection,
role, original request, and explicit clarification. Implementer input also identifies
the exact plan body and producing task/attempt; input validation does not establish
acceptance. Hosts must verify acceptance before dispatch. Assignment and original inputs
remain fixed across attempts, while new clarification may be supplied.

Use `SessionService::submit_agent_task` to admit the request through the session's
serial worker. Execution uses the prepared session-owned Env and enforces role
permissions. Planner turns are read-only and skip mutating host effects. Implementer
turns retain session permissions and finish permitted commit and publication effects
before the next turn; failures settle the attempt as failed. A fresh regular `Draft` can
start with a Planner without an ordinary chat turn. Other role attempts require `Review`
or `AgentReview`; unmaterialized deferred workspaces are rejected before model
execution. Role admission also invalidates prior ordinary review generations. A
successful Implementer attempt without questions starts the ordinary automatic review
after its effects, as an ordinary turn would. Planner, question, failed, canceled, and
superseded attempts start no automatic review; explicit review remains available.

`TaskRepository` can atomically bind that assignment and a host-allocated conversation
identity when admitting a role-aware attempt. The binding and original inputs survive
restart; clarification belongs to each attempt. Tasks with ordinary session-context
history cannot acquire an isolated assignment, and assigned tasks require role-aware
delivery.

Task-owned history stores each observed response with its producing attempt and keeps
provider continuation separate from ordinary session chat. Saving a response accounts
usage once under the task's captured model; repeated saves cannot overwrite output or
rewind a newer conversation. A response can be available while post-processing is still
running, so callers must inspect the attempt status before accepting it. History alone
does not complete a task. Use `SessionService::get_agent_task_history` to retrieve its
assignment, explicit inputs, responses, and usage without reading ordinary chat.

`TaskRepository::accept_agent_plan` explicitly accepts one persisted Planner attempt.
Only the latest successful, uncanceled response with a nonempty plan and no questions is
eligible. The acceptance ledger retains its exact answer; generic task completion does
not accept a plan. Implementer admission checks the ledger, session, original request,
and captured model in the same transaction as the attempt. A newer Planner attempt makes
an older version ineligible for new handoffs, while preserving its historical acceptance
record. Repeated acceptance of the same eligible version is idempotent.

## Static Plan Then Implement Workflow

A static workflow captures one original request and provider/model selection for two
isolated tasks sharing the session's workspace. Planning finishes before explicit plan
acceptance admits implementation. Revision and clarification use new attempts in the
current role's context; implementation retains the accepted plan exactly.

Workflow phase is projected from its current attempt and saved response. A saved plan
does not become acceptable until the entire attempt, including post-processing, has
succeeded. Cancellation affects only the workflow's operations. Identical launch,
acceptance, and continuation deliveries acknowledge their original receipts even after
the stage changes; they never restart work or clear cancellation. An explicit new
attempt is required to retry a canceled, failed, or interrupted stage. Reading or
reopening a workflow never replays implementation.

The session transcript shows the workflow without sharing role context. A newly admitted
launch records the original request and an execution-graph marker, and each newly saved
role response appends one answer attributed to its role. Repeated deliveries and saves
never duplicate these rows, and ordinary provider replay excludes the role answers and
marker.

`SessionService` exposes this optional workflow capability through
`start_plan_then_implement`, `get_task_chain`, `list_task_chains`,
`accept_task_chain_plan`, `continue_task_chain`, and `cancel_task_chain`. The SQLite
adapter admits each action atomically; hosts dispatch newly admitted requests through
the session's serialized worker.

Continuations include `expected_attempt_id`, captured from the workflow snapshot when
guidance is authored. Admission rejects stale guidance if another action has advanced
the workflow. The continuation receipt retains that source identity, so exact repeats
remain safe after later progress; changing the source under an existing delivery ID or
reusing a launch or acceptance ID as a continuation is rejected.

### Natural-Language Replies

`SessionService::reply_to_task_chain` sends a user reply to the session's latest
workflow. The host, not the caller, decides what the reply means; no words are reserved.
The reply is stored once as a visible user prompt before any agent answer. The host then
routes it by the phase observed at that moment:

- While a plan awaits acceptance, an orchestrator decision runs as a read-only utility
  [run](@/docs/core-components/durable-components/run.md) with the workflow's captured
  model. Only a clear approval with no requested changes accepts that exact plan and
  starts the Implementer, followed by a short workflow notice. Anything else, including
  approval with changes or a failed or unparsable decision, becomes revision guidance
  for the Planner.
- After a question, failure, cancellation, or success, the reply continues the current
  role. Once implementation starts, that role is always the Implementer.
- While an agent is working or an earlier reply is still being decided, the reply is
  rejected with `SessionError::TaskChainBusy`. A session without a workflow returns
  `SessionError::TaskChainNotFound`.

The result is `PlanAccepted`, `PlanRevised`, or `Continued`. Each action uses an attempt
ID derived from the caller's globally unique `reply_id`. Redelivering a reply returns
its original outcome without recording, deciding, or admitting work again. Reusing that
identity with different text is rejected. Canceling the workflow interrupts its pending
decision; a late decision cannot restart canceled work.

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

For ordinary tasks, a clarification response finishes its attempt and leaves the task
open and the session waiting for input. Read the session's questions and submit a new
attempt with the answers in its message to continue task accounting. Ordinary question
answers retain their existing session behavior and are not automatically attached to a
task.

Role-aware clarification stays in that task's response history and does not replace the
ordinary session's pending questions. Submit a new role-aware attempt with explicit
`TaskAgentInput::clarification` to continue the task's own conversation.

Use a new attempt ID for an explicit retry or continuation. Provider retries remain
inside the current attempt. Only one attempt per task may be unfinished; dependency
scheduling and automatic retries are not provided.

Session cancellation settles its attempts through ordinary operation cancellation and
leaves the desired outcome open. Restart recovery marks unfinished operations and their
attempts failed without replaying them. Earlier attempts retain their identities and
history. Deleting the owning session deletes its tasks and attempts. Utility
[runs](@/docs/core-components/durable-components/run.md) remain separate records.
