+++
title = "Command Gateway"
description = "Shared admission for terminal and programmatic session commands."
weight = 3
+++

The Command Gateway gives terminal and programmatic callers a shared entry point for
session workflow mutations. For ordinary messages, a pure application-layer planner
selects staging, starting, queueing, or resuming from submission intent and session
state. The gateway applies that plan through existing workflow handlers, which retain
their validation, persistence, and execution responsibilities. Other commands dispatch
directly to their existing handlers.

## Commands and Identity

The gateway accepts session creation, prepared terminal prompts, ordinary and durable
messages, question answers, cancellation, staged starts, merge, and review publication.
It also accepts [task](@/docs/core-components/durable-components/task.md) creation,
attempt submission, and explicit completion. Session and task reads remain separate
queries.

Each request carries a fresh command ID and an initiator: terminal or session API.
Durable messages, task attempts, and review publication also carry an operation
correlation. The command ID identifies one admission attempt; the durable operation ID
retains its existing deduplication semantics. Initiator metadata records provenance
rather than granting permissions.

## Delivery and Acknowledgement

The foreground task owns admission and application state. Programmatic callers use the
bounded session-runtime mailbox. Terminal actions can enter directly or use that same
mailbox; actions using the mailbox retain their order behind previously accepted
requests, while keeping terminal provenance.

Command payloads contain application inputs, separate from response channels. The
delivery envelope pairs each payload with its acknowledgement contract: admission,
prepared creation, or publication completion. Terminal composer modes become explicit
stage, start, or follow-up intent before admission; cancellation-only composer details
do not enter the message planner.

Acknowledgement follows the workflow. Ordinary message submissions return after
acceptance, while API creation of a materialized session waits for workspace
preparation. Terminal review publication returns after admission; API publication waits
for the worker's result. A task-chain reply returns its routed outcome; while a plan
awaits acceptance, it waits for the orchestrator decision and the admission of the
decided action. An accepted command does not necessarily mean its work has completed.

Terminal prompts retain their draft-staging intent and attachments. API messages retain
their automatic start behavior. Busy ordinary chat uses an in-memory queue, while
durable submissions enter the serialized worker directly. See
[Message](@/docs/core-components/durable-components/message.md) for transcript storage
and visibility.

## Execution Boundary

Command admission routes requested actions; the
[Scheduler](@/docs/core-components/scheduler.md) grants capacity to eligible model
turns. Workers retain per-session ordering through post-processing, cancellation, and
recovery. Model work follows
[Run Worker → Agent Runtime → Harness → LLM](@/docs/core-components/execution.md).
