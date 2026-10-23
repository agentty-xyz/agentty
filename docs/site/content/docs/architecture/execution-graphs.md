+++
title = "Execution Graphs"
description = "Validated graph snapshots, durable results, and exact handoffs."
weight = 8
+++

`ag-session` provides pure contracts for arbitrary static execution graphs. `ag-store`
persists their revisions, execution links, and exact result versions. Graph dispatch and
recovery are not integrated; `/plan` still uses `TaskChain`, and focused review retains
its existing coordinator.

## Graph and Node Identity

An `ExecutionGraph` has an instance ID, a nonzero revision, and one owning session. Node
IDs are unique within the graph. Each node captures its agent definition and model
selection, objective, initial request, execution profile, Env reference, dependencies,
and named inputs and outputs. Definitions supply reusable instructions independently of
built-in workflow roles; the captured request supplies this node's work and evidence.
Requests use `TurnPrompt` to retain image attachments and distinguish user text from
generated evidence.

Validation rejects missing or duplicate references, incompatible execution profiles,
foreign Envs, and cycles, including disconnected cycles. It preserves caller order;
declaration order does not establish execution order. Current Env references remain
limited to the owning session's shared local worktree.

## Execution Requirements

`GraphContext::TaskConversation` reserves a distinct task per node. Its attempts retain
that task's isolated history, use the session-turn response schema, and reference
existing task-attempt operations. `GraphContext::Fresh` starts an independent utility
context for each run and uses an existing utility response schema, such as
`FocusedReview`. It references the existing utility-run record. Neither path creates
another execution status.

`SharedRead` requires read-only execution and no host mutation effects. Multiple readers
may share an Env. `ExclusiveWrite` requires serialization against every other operation
in that Env, including readers; it cannot relax session permissions, definition
restrictions, or worker policy. Ordered session effects are explicit and remain part of
execution settlement. Read-only definitions cannot request those effects.

A shared budget ID names a host-owned provider-call budget covering retries and protocol
repairs. The contract does not allocate or enforce that budget. Hosts must reject
unsupported requirements and continue submitting all model work through
[workers and runtime](@/docs/core-components/execution.md).

## Gates and Exact Handoffs

Dependencies explicitly require settled, question-free execution, a validated named
output, or user acceptance of that output. Successful provider execution alone does not
satisfy a result gate or complete a task's desired outcome. Multiple prerequisites form
a join; every gate must be satisfied.

A planned `GraphInputBinding` names its producer and output without requiring a result
to exist. It must have a matching validation or acceptance dependency. Each named output
represents a complete response under the node's protocol schema; arbitrary output
schemas and field projections are not supported.

At admission, `ExecutionGraph::bind_input` checks a host-loaded result and returns a
`GraphResolvedInput`. Its immutable reference identifies the graph revision, node,
producing task attempt or utility run, output name, schema, result identity, and
version. An accepted version stays distinct from later output. Redelivery must retain
the captured reference rather than query for the latest result again.

`Produced`, `Validated`, and `Accepted` describe host-attested output state, separately
from execution status. Validation requires successful settlement with no pending
questions; acceptance selects that exact validated version. Caller-constructed
references and state are not evidence of persistence, validation, or user approval.

## Durable Snapshots and Evidence

`GraphRepository`, available through `AppRepositories::graphs()`, saves a complete
validated snapshot before any execution exists. It retains node definitions, requests
and attachments, dependencies, profiles, Env references, and planned bindings. Revisions
start at one and advance with an expected-current-revision check. Identical historical
deliveries are idempotent; conflicting contents or session ownership are rejected. Once
executions are linked, further revisions are rejected until safe mutation policy is
implemented.

Execution links reference existing task attempts and their operations, or session-owned
utility runs. Linking checks ownership and captures every declared input against stored
result evidence in one transaction. It does not admit or dispatch work, enforce capacity
or Env access, or establish dependency gates unrelated to those inputs. A coordinator
must combine those checks with dispatch admission.

Each execution can publish one immutable body per declared output. Consecutive versions
retain separate identities and producing execution references. Validation records host
evidence only after successful settlement, protocol parsing, and clarification checks.
Acceptance separately records an explicit host-attested action on that exact validated
version. Evidence retains its first timestamp; conflicting redelivery is rejected. These
APIs are trusted host boundaries, not model-callable approval tools.

Result state is projected from evidence; execution state remains authoritative in the
existing operation or utility run. Later results never retarget saved inputs or inherit
acceptance. Reopening the database restores snapshots and receipts without replaying
work or completing tasks. Deleting the owning session removes its graph records.

## Host Integration Boundary

A future coordinator must combine authoritative gates, graph revisions, and exact input
capture atomically with dispatch admission. It must enforce Env conflicts, shared
budgets, worker capacity, cancellation, and explicit restart recovery. Storage provides
no automatic replay or permission to revise started work. Graph size limits, dynamic
proposals, and runtime expansion remain coordinator policy to implement separately.
