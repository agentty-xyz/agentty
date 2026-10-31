+++
title = "Execution Graphs"
description = "Validated graph snapshots, durable results, and exact handoffs."
weight = 8
+++

`ag-session` provides pure contracts for arbitrary static execution graphs. `ag-store`
persists their revisions, execution links, and exact result versions. `ag-worker`
provides a deterministic static executor with atomic storage admission. Agentty adapts
task conversations to its serial session workers and fresh utilities to the concurrent
utility worker. Focused review and `/plan` both run on this executor. `TaskChain` builds
and projects the fixed Planner-to-Implementer graph; it has no separate execution
ledger.

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
may share an Env. `BackgroundRead` is a read-only fresh utility that may also overlap
ordinary session work, such as turns and sync, but never graph writers; use it only when
observing a changing workspace is acceptable. `ExclusiveWrite` requires serialization
against every other operation in that Env, including both kinds of readers; it cannot
relax session permissions, definition restrictions, or worker policy. Ordered session
effects are explicit and remain part of execution settlement. Read-only definitions
cannot request those effects.

A shared budget ID names a host-owned provider-call budget covering retries and protocol
repairs. The static executor resolves that ID to the same shared counter for every
consumer and rejects unknown budgets. Before durable admission, it atomically reserves
the initial provider call and transfers that reservation through worker execution.
Blocked or existing admissions return unused reservations. If no call is available, the
node remains unclaimed until a later call supplies a fresh budget. The first provider
attempt redeems its reservation; retries charge the shared counter. Hosts must reject
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

## Static Dispatch

`GraphExecutor::run_ready` drains ready nodes in declaration order, running compatible
branches concurrently and reevaluating joins after settlement. Each node requires every
dependency gate. `GraphDispatchRepository` checks the current revision, gates, Env
conflicts, and exact inputs in the same transaction that creates the operation-backed
task attempt or utility run. Competing admissions return the original receipt without
dispatching again, including after a restart or failed execution.

The executor intersects session and definition permissions and retains worker policy and
capacity ceilings. Utility nodes retain their Env lease through settlement. Session
adapters publish active ownership before waiting for Env access after serial selection,
so concurrent sync queues behind the turn. Queued nodes cannot hold a lease needed by
earlier work. Gate waits observe cancellation, and completion retains acquired leases
through ordered effects and operation settlement. Hosts share one `GraphResources` pool
with other work using those Envs; the database also excludes conflicting active
operations and utilities across graphs. Dropping a driver leaves admitted work owned
through cleanup. Inherited cancellation still reaches workers. Runner panics follow the
same durable failure path as returned errors before resource leases are released.

Successful settlement saves the complete schema-valid response, even for nodes without
named outputs. Question-free outputs receive validation evidence; user acceptance and
desired-outcome task completion remain separate actions. A failed or clarification
response cannot release an execution-success gate. A wave drains admitted peers before
reporting a failure. Each settled node releases its capacity and Env lease before
notifying other graph drivers, so available resources can be reused while unrelated
peers are still running. Failed nodes signal release through the same path.
Resource-blocked drivers also receive targeted wakeups when an unclaimed admission check
returns its capacity or Env lease. Hosts wait on `GraphExecutor::capacity_released` and
`GraphExecutor::environment_released` alongside `GraphResources::subscribe_releases`.
Idle scans do not broadcast resource changes to one another. They reuse capacity across
unclaimed checks, so competing drivers cannot repeatedly interrupt and restart each
other's idle scans. `GraphExecutor::with_wave_limit` bounds the nodes one wave admits
below the shared capacity; each wave still settles before the next begins.

A fresh node may coordinate several child utility calls through
`RunWorker::coordinate_recorded`. The node's preadmitted run is claimed before the work
starts, receives heartbeats, and settles after it returns. It holds no utility slot, so
its children never queue behind it; they record the node's run as their parent.
Cancellation of the session, worker, or caller drops the work and its children. The node
settles only after every child, including canceled or timed-out ones, finishes cleanup,
so its Env lease still excludes graph writers during that cleanup.

The supplied `GraphUtilityRunner` supports fresh text, focused-review, and
review-metadata nodes through the existing utility worker, retaining their preadmitted
identities. It rejects attachments; explicitly launched graphs admit fresh nodes only as
shared readers, routing writers through task conversations and session stack policy.
Agentty's task adapter retains attachments, captured instructions and model selection,
and exact bound inputs. Each static task starts an isolated conversation using its
durable attempt UUID for runtime resources. Observed responses, provider continuation,
and usage persist before effects; failed effects cannot publish a validated output.
Ordinary chat history, provider continuation, and pending questions remain separate.
Task identities must be unused at first admission.

`SessionService::start_execution_graph` explicitly saves and starts a static graph in a
prepared session. Named provider-call limits remain shared across the launch, including
retries, repairs, and ordered utility children. Identical active launches do not refill
those limits. Acceptance uses `accept_execution_graph_result` with an exact validated
reference. Acceptance and Env release wake the active drivers; launch-time snapshots are
never discovered and replayed automatically at startup. Graph writers exclude ordinary
session work, merge workflows, and focused-review readers.

## Focused Review Graph

Each focused-review generation launches its own static graph. History summarization,
when needed, runs first under a background-reader lease. The captured diff is then split
exactly as the prompt budget would reject it, without provider calls, into one node per
fragment. A cross-file node joins every fragment's validated review, and a consolidation
node joins both phases through exact pinned inputs. Every node is a `BackgroundRead`
fresh utility, so reviews still overlap ordinary session work and conflict-free sync.

Fragment nodes run in waves of at most three, and at most three provider calls run at
once across the whole review, including size-driven subdivision. A failure drains its
running peers and stops later waves; completed findings remain in the partial review.
Nodes coordinate their phase's existing retries, subdivision, and history repairs. The
review budget is enforced by those phases rather than reserved per node, so exhaustion
is reported by the phase that ran out. The review deadline bounds provider calls, Env
waits, and dispatch waits. Each active review registers as a graph driver:
`cancel_execution_graph` or session cleanup cancels its running calls, and an
interrupted fragment stays unfinished. Fragment text stays with the host instead of the
snapshot. An incomplete generation's graph is canceled; retries start a new graph and
reuse successful calls through review checkpoints.

## Lifecycle and Explicit Recovery

`SessionService::get_execution_graph` returns node progress, unsatisfied dependency
gates, exact outputs, clarification questions, execution history, and failure details.
It also reports whether dispatch requires explicit resume or a named live budget is
exhausted. An empty dispatch result never means completion. A graph completes only when
every node has settled successfully without questions; this does not accept task
outcomes or automatically complete their tasks.

`cancel_execution_graph` closes durable admission before signaling the graph's workers.
Cancellation is terminal for that graph identity, including nodes not yet started.
Already completed graphs retain their completion state. Workers retain their resources
through cleanup; unrelated session work is unaffected. Late responses and acceptance
actions cannot advance a canceled graph.

`continue_execution_graph_node` records a retry, complete ordered clarification answers,
or a free-form reply to a settled task turn. A reply may revise an unaccepted output or
follow up on a terminal node; accepted outputs and nodes with started dependents are
final. Reply images remain attached to later attempts. The request names the current
revision, exact source execution, and a new execution UUID. A retry requires failed,
canceled, or incompletely settled work. Clarification continues the same isolated task
with a new attempt and its own runtime context, replaying only that node's retained
responses and explicit replies. Neither action rewrites previous attempts or changes
their pinned inputs. Identical delivery never executes twice; conflicting identities and
stale sources are rejected. Failed predecessors keep dependent nodes blocked while
independent branches may proceed.

Startup restores records without dispatching them. `resume_execution_graph` explicitly
starts dispatch for the saved revision; admitted nodes require a separate continuation
action before another execution. Live launches retain their original shared limits and
remaining credits across resume calls. After process restart, explicit resume supplies
new limits for that live execution period; budget counters are not durable usage
ledgers. Recording continuation intent alone never recreates a driver or replenishes
credits. Graph proposals and runtime expansion remain separate work.

## Plan Then Implement Graph

`/plan` saves two task-conversation nodes with captured definitions, model, attachments,
and the Planner's launch-time session snapshot. The Planner is a shared reader. Its
validated `plan` output requires explicit acceptance before the exclusive-writer
Implementer can start with that exact result pinned as input. Planner revision and
clarification create new attempts in the same isolated history. The Implementer retains
the original images and the accepted Planner attempt's reply images, and settles ordered
session effects before its execution succeeds.

Workflow phase and the visible graph card project the graph's operation-backed state.
Natural-language replies retain their exact source execution and first decision before
applying an idempotent graph action. Canceling a stage leaves the graph open for an
explicit retry; whole-graph cancellation remains terminal. Reading saved workflows never
dispatches work.
