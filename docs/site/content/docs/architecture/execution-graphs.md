+++
title = "Execution Graphs"
description = "Validated graph snapshots, durable results, and exact handoffs."
weight = 8
+++

`ag-session` provides pure contracts for static execution graphs. `ag-store` persists
their revisions, execution links, and exact result versions. `ag-worker` provides a
deterministic static executor with atomic storage admission. Agentty adapts task
conversations to its serial session workers and fresh utilities to the concurrent
utility worker. Focused review and approved `/graph` proposals both run on this
executor.

## Graph and Node Identity

Model proposals are separate from executable snapshots. The host captures the original
request, agent catalog, model, permission ceiling, and bounded generation and execution
limits. A read-only utility run produces a clarification or a logical proposal; pure
resolution rejects unsupported profiles and permission escalation, allocates task
identities, binds the session Env, and validates the graph. Approving an exact version
publishes the resolved snapshot, its receipt, and its limits in one transaction before
dispatch. Restoring a proposal or repeating an approval never replenishes limits or
starts execution. Chat replies to a pending proposal are bound to the exact version and
classified once by a read-only decision run. See
[the proposal workflow](@/docs/usage/workflow.md#generated-graph-proposals).

An `ExecutionGraph` has an instance ID, a nonzero revision, and one owning session. Node
IDs are unique within the graph. Each node captures its agent definition and model
selection, objective, request, execution profile, Env reference, dependencies, and named
inputs and outputs. Requests use `TurnPrompt` to keep image attachments and to
distinguish user text from generated evidence.

Validation rejects missing or duplicate references, incompatible execution profiles,
foreign Envs, and cycles. Declaration order does not establish execution order. Env
references are currently limited to the owning session's shared local worktree.

## Execution Requirements

`GraphContext::TaskConversation` reserves a distinct task per node; its attempts keep
that task's isolated history and use existing task-attempt operations.
`GraphContext::Fresh` starts an independent utility context per run and uses the
existing utility-run record. Neither path adds an execution status.

Workspace access is explicit:

- `SharedRead` is read-only; multiple readers may share an Env.
- `BackgroundRead` is a read-only fresh utility that may also overlap ordinary session
  work, such as turns and sync, but never graph writers. Use it only when observing a
  changing workspace is acceptable.
- `ExclusiveWrite` serializes against every other operation in that Env. It cannot relax
  session permissions, definition restrictions, or worker policy.

A shared budget ID names a host-owned provider-call budget that covers retries and
protocol repairs. The executor resolves it to one shared counter, reserves the first
provider call before durable admission, and refunds unused reservations. A node with no
available call stays unclaimed. All model work still follows the
[execution boundary](@/docs/core-components/execution-components/execution-boundary.md).

## Gates and Exact Handoffs

Dependencies require settled, question-free execution, a validated named output, or user
acceptance of that output. Successful provider execution alone does not satisfy a result
gate or complete a task. Multiple prerequisites form a join; every gate must hold.

A `GraphInputBinding` names its producer and output and must have a matching validation
or acceptance dependency. Each named output is a complete response under the node's
protocol schema; field projections are not supported. At admission,
`ExecutionGraph::bind_input` resolves an immutable reference to the exact graph
revision, node, producing execution, output, and version. Redelivery keeps that
reference instead of querying the latest result again.

`Produced`, `Validated`, and `Accepted` describe host-attested output state, separate
from execution status. Validation requires successful settlement with no pending
questions; acceptance selects that exact validated version.

## Durable Snapshots and Evidence

`GraphRepository` saves a complete validated snapshot before any execution exists.
Revisions start at one and advance with an expected-current-revision check. Identical
redeliveries are idempotent; conflicting contents or ownership are rejected. Once
executions are linked, further revisions are rejected.

Execution links reference existing task attempts or session-owned utility runs. Linking
checks ownership and captures every declared input against stored result evidence in one
transaction. Each execution can publish one immutable body per declared output, and
consecutive versions keep separate identities. Validation and acceptance evidence are
trusted host records, not model-callable approval tools.

Execution state stays authoritative in the existing operation or utility run. Later
results never retarget saved inputs or inherit acceptance. Reopening the database
restores snapshots and receipts without replaying work. Deleting the owning session
removes its graph records.

## Static Dispatch

`GraphExecutor::run_ready` drains ready nodes in declaration order, running compatible
branches concurrently and reevaluating joins after settlement. `GraphDispatchRepository`
checks the current revision, gates, Env conflicts, and exact inputs in the same
transaction that creates the operation-backed task attempt or utility run. Competing
admissions return the original receipt without dispatching again, including after a
restart.

The executor intersects session and definition permissions and keeps worker policy and
capacity ceilings. Hosts share one `GraphResources` lease pool with other work in those
Envs, and the database also excludes conflicting active work across graphs. Session
adapters publish active ownership before waiting for Env access, so concurrent sync
queues behind the turn. Leases are held through ordered effects and settlement; each
settled node releases its capacity and Env lease and wakes only the drivers waiting for
them. Dropping a driver leaves admitted work owned through cleanup, and runner panics
follow the same durable failure path as returned errors.

Successful settlement saves the complete schema-valid response. Question-free outputs
receive validation evidence; user acceptance and task completion remain separate
actions. A wave drains admitted peers before reporting a failure.

A fresh node may coordinate several child utility calls through
`RunWorker::coordinate_recorded`. The node holds no utility slot, so its children never
queue behind it, and it settles only after every child finishes cleanup, keeping its Env
lease until then.

Explicitly launched graphs admit fresh nodes only as shared readers, routing writers
through task conversations and session stack policy. Each task node starts an isolated
conversation; its responses, provider continuation, and usage persist before effects,
and failed effects cannot publish a validated output. Ordinary chat history and pending
questions remain separate.

`SessionService::start_execution_graph` saves and starts a static graph in a prepared
session. Named provider-call limits are fixed at first launch and every charge and
refund is written to a durable ledger, so no relaunch refills spent calls. Acceptance
uses `accept_execution_graph_result` with an exact validated reference. Graph writers
exclude ordinary session work, merges, and focused-review readers.

## Focused Review Graph

Each focused-review generation launches its own static graph. Optional history
summarization runs first. The captured diff is split into one node per fragment without
provider calls; a cross-file node joins every fragment's validated review, and a
consolidation node joins both phases through exact pinned inputs. Every node is a
`BackgroundRead` fresh utility, so reviews overlap ordinary session work and
conflict-free sync.

Fragment nodes run in bounded waves with a shared cap on concurrent provider calls. A
failure drains running peers and stops later waves; completed findings remain in the
partial review. The review deadline bounds provider calls and waits, and cancelling the
graph or cleaning up the session cancels running calls. Fragment text stays with the
host, not the snapshot. Retries start a new graph and reuse successful calls through
review checkpoints.

## Lifecycle and Explicit Recovery

`SessionService::get_execution_graph` returns node progress, unsatisfied gates, exact
outputs, clarification questions, execution history, failures, and whether dispatch
needs an explicit resume or a budget is exhausted. A graph completes only when every
node has settled successfully without questions; this does not accept outputs or
complete tasks.

`cancel_execution_graph` closes durable admission before signaling workers and is
terminal for that graph, including nodes not yet started. Workers keep their resources
through cleanup, and late responses or acceptances cannot advance a canceled graph.

`continue_execution_graph_node` records a retry, complete clarification answers, or a
free-form reply to a settled task turn against the current revision and exact source
execution, with a new execution ID. Accepted outputs and nodes with started dependents
are final. Continuations never rewrite earlier attempts or their pinned inputs, and
identical delivery never executes twice.

Startup restores records without dispatching them. `resume_execution_graph` explicitly
restarts dispatch for the saved revision, starting from the limits fixed at first launch
minus recorded spending. Recording continuation intent alone never recreates a driver or
replenishes budgets.
