+++
title = "Execution Graphs"
description = "Validated graph snapshots, durable results, and exact handoffs."
weight = 8
+++

`ag-session` provides pure contracts for static execution graphs. `ag-store` persists
their revisions, execution links, and exact result versions. `ag-worker` provides a
deterministic static executor with atomic storage admission. Agentty adapts writing task
conversations to its serial session workers, read-only task conversations to a lane
beside them, and fresh utilities to the concurrent utility worker. Focused review and
approved `/graph` proposals both run on this executor.

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
selection, objective, request, execution profile, Env reference, dependencies, named
inputs and outputs, and an optional parent. Requests use `TurnPrompt` to keep image
attachments and to distinguish user text from generated evidence.

Validation rejects missing or duplicate references, incompatible execution profiles,
foreign Envs, invalid parents, and cycles. Declaration order does not establish
execution order. Env references are currently limited to the owning session's shared
local worktree.

## Node Hierarchy

A node may name a top-level node of the same graph as its parent, so a graph has at most
two levels. A child is a step of its parent's deliverable and shares its parent's Env;
proposal resolution binds it to the parent's concrete Env rather than recording a
separate Env kind, and reads a blank proposed parent as none. Validation rejects a
missing parent, a node naming itself, and a parent that is itself a child, each with a
reason that names the fix; it also rejects a blank parent and a child bound to another
Env, which only a host building a graph itself can produce.

The hierarchy constrains shape and Env and drives presentation: the terminal's graph
views show each top-level node followed by its children, with the parent's progress
across its children. It adds no execution semantics. A child is an ordinary node with
ordinary dependencies and gates, and a dependency waits only for the node it names: a
child waits for its parent only through an explicit dependency, and a parent, or a node
depending on it, never waits for its children. Nodes without a parent omit the field, so
snapshots and expansion records of graphs without children keep their stored bytes.

## Execution Requirements

`GraphContext::TaskConversation` reserves a distinct task per node; its attempts keep
that task's isolated history and use existing task-attempt operations.
`GraphContext::Fresh` starts an independent utility context per run and uses the
existing utility-run record. Neither path adds an execution status. Each running node
holds a session-status lease, so the session shows `InProgress` until the dispatch pass
that ran it settles; the lease's recovery operation never blocks graph claims.

Workspace access is explicit:

- `SharedRead` is read-only; multiple readers may share an Env.
- `BackgroundRead` is a read-only fresh utility that may also overlap ordinary session
  work, such as turns and sync, but never graph writers. Use it only when observing a
  changing workspace is acceptable.
- `ExclusiveWrite` serializes against every other operation in that Env. It cannot relax
  host permission ceilings, definition restrictions, or worker policy.

These rules apply within one Env; work in different Envs never conflicts. Session work
that is not a graph node, such as chat turns, sync, merges, publication, and session
utilities, holds the session worktree.

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
executions are linked, only [expansions](#controlled-expansion) add revisions.

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
checks the current revision, gates, conflicts in the node's Env, and exact inputs in the
same transaction that creates the operation-backed task attempt or utility run.
Competing admissions return the original receipt without dispatching again, including
after a restart.

The host's `GraphEnvResolver` places each node. After reserving the node's first
provider call, the executor asks the resolver where the node would run, without any
durable effect, and passes that location to the claim; a fresh utility run records it. A
node whose Env cannot be offered stays unclaimed and returns its call until the host
wakes its drivers. A live executor prepares the Env at most once per claim before the
runner starts, and a failed or panicking preparation fails the execution like a runner
error. The executor passes the location through without inspecting it. Agentty's
resolver places every node in its session worktree.

The executor intersects the host ceiling with definition permissions and keeps worker
policy and capacity ceilings. Hosts share one `GraphResources` lease pool with other
work in those Envs, and the database also excludes conflicting active work in the same
Env across graphs. Session adapters publish active ownership before waiting for Env
access, so concurrent sync queues behind the turn. Leases are held through ordered
effects and settlement; each settled node releases its capacity and Env lease and wakes
only the drivers waiting for them. Dropping a driver leaves admitted work owned through
cleanup, and runner panics follow the same durable failure path as returned errors.

Successful settlement saves the complete schema-valid response. Question-free outputs
receive validation evidence; user acceptance and task completion remain separate
actions. A wave drains admitted peers before reporting a failure.

A run turns terminal before settlement stores its response. Hosts therefore report a
node as running while its live executor still settles it, sampling
`GraphExecutor::unsettled_executions` before reading progress. A finished run without a
response that no live executor owns, such as after a restart or a failed store, is
failed and needs an explicit retry.

A fresh node may coordinate several child utility calls through
`RunWorker::coordinate_recorded`. The node holds no utility slot, so its children never
queue behind it, and it settles only after every child finishes cleanup, keeping its Env
lease until then.

Explicitly launched graphs admit fresh nodes only as shared readers, routing writers
through task conversations. Each task node starts an isolated conversation, which also
reads the [session document](#session-document); its responses, provider continuation,
and usage persist before effects, and failed effects cannot publish a validated output
or session notes. Ordinary chat history and pending questions remain separate.

Writing task nodes run on the session's serial worker, so their effects stay ordered
with chat turns, sync, and publication. Read-only task nodes run beside it, so
independent readers overlap each other instead of queuing. Their shared Env lease still
excludes writers and ordinary session work, which holds the Env exclusively. A reader
leaves session status to its node's graph lease, so it neither waits for nor hides a
pending chat question. Stopping the session cancels running readers too.

`SessionService::start_execution_graph` saves and starts a static graph in a prepared
session. Named provider-call limits are fixed at first launch and every charge and
refund is written to a durable ledger, so no relaunch refills spent calls. Only
`extend_execution_graph_budget` adds calls: it records one idempotent extension per
action ID, refuses canceled or completed graphs, and credits a retained driver's live
budget after the record commits. Launch limits stay unchanged, so a proposal graph still
relaunches against its approved limits. Acceptance uses `accept_execution_graph_result`
with an exact validated reference. Graph writers in the session worktree exclude
ordinary session work, merges, and focused-review readers.

## Controlled Expansion

`GraphRepository::expand_graph` appends nodes as the next revision of a launched graph.
`ExecutionGraph::expand` keeps every existing node unchanged; appended nodes may depend
on existing ones, may be children of existing top-level nodes, and pass the usual
validation. The change is checked against the expected current revision and is
idempotent by its expansion ID. Canceled and completed graphs are refused, and appended
nodes must spend from the graph's existing budgets, so an expansion never adds provider
calls.

Because no node changes, work addressed to an earlier revision that only expansions
followed continues under the current one: admission, settlement, acceptance,
continuations, and history treat each node's executions across those revisions as one
sequence, and inputs pinned under an earlier revision stay exact. A full revision ends
that lineage. `GraphExecutor::run_ready` reloads the graph every wave, so a live driver
dispatches appended nodes once woken.

Expansion starts only from an explicit user request. A `GenerateGraphProposalRequest`
names no target: for every frontend, the host expands the graph that the session's
newest approved proposal launched while it can still run, captures its current
revision's snapshot, and a read-only run proposes new nodes against it. Otherwise the
request proposes a new graph. Approving the exact version appends them in the approval
transaction, then wakes the retained driver or resumes a stopped one.

## Session Document

Each session keeps one
[session document](@/docs/core-components/durable-components/session-document.md) of
shared notes that the task nodes of all its graphs read and add to.
`SessionDocumentRepository` writes it one transaction at a time: an append takes the
next version, so overlapping nodes lose no note, and replacing the body requires the
expected current version.

At admission, a task-conversation node's prompt carries the document's entries since its
newest replacement, not its rendered body: a fenced JSON array in which the host records
each entry's version, author, and kind, and only `text` is the author's, with characters
that could close a tag or form a mention or image placeholder escaped. No note can
therefore forge a heading, an author, or a host remark. The entries fit a 16 KiB budget
counted on each entry's raw text plus a fixed overhead, so escaping evicts nothing. Past
it, the oldest entries are left out whole, except that a replacement opening the entries
keeps its newest part; the host text says what it left out and whether the document is
full. Sentences before and after the block say it is reference data, never instructions.
A document that cannot be read fails no node: the node runs without the section, and one
warning per graph says so.

The same prompt invites up to five notes through the response's optional `notes` field,
which every other turn leaves empty and every host ignores. Notes stay out of the saved
response and the output that bound consumers read. The host publishes them after the
turn's effects succeed, when it asked no questions, and before the node settles. A turn
that fails or is canceled before then, asks questions, or whose effects fail publishes
nothing; a cancel or failure while the node settles can leave published notes in place.
Each retry or continuation of a node is a new execution, which may publish similar notes
again. Storage strips characters that render as nothing from every note and body, and
attributes each note to the node's execution. Publication runs in one transaction, and
an execution that already wrote an entry adds nothing more, so a redelivered turn adds
nothing twice, even with different notes, and repeats no warning. Notes the document
refuses become one session notice, and a document that cannot be written leaves a
warning instead of failing the node. A crash between the saved response and the
publication loses that turn's notes. Fresh utility nodes neither read the document nor
add to it. `SessionService::get_session_document` reads it for hosts.

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
plus recorded extensions minus recorded spending. Recording continuation intent alone
never recreates a driver or replenishes budgets.

A chat reply to an output awaiting acceptance is saved with the exact output version and
execution it answered, and a read-only decision run classifies it once. A reply that a
restart left unapplied is applied at startup and resumes its graph, as the user's
explicit request to continue; revision guidance keeps the reply's identity, so
redelivery never continues the node twice. A reply is not recovered once its output
version was accepted or replaced by a newer version.
