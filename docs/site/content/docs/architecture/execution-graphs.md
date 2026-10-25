+++
title = "Execution Graphs"
description = "Validated graph snapshots, durable results, and exact handoffs."
weight = 8
+++

`ag-session` provides pure contracts for arbitrary static execution graphs. `ag-store`
persists their revisions, execution links, and exact result versions. `ag-worker`
provides a deterministic static executor with atomic storage admission. Agentty adapts
task conversations to its serial session workers and fresh utilities to the concurrent
utility worker. `/plan` still uses `TaskChain`, and focused review retains its existing
coordinator; neither workflow has migrated to this executor.

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
other's idle scans.

The supplied `GraphUtilityRunner` supports fresh text, focused-review, and
review-metadata nodes through the existing utility worker, retaining their preadmitted
identities. It rejects attachments; Agentty currently admits fresh nodes as shared
readers, routing writers through task conversations and session stack policy. Agentty's
task adapter retains attachments, captured instructions and model selection, and exact
bound inputs. Each static task starts an isolated conversation using its durable attempt
UUID for runtime resources. Observed responses, provider continuation, and usage persist
before effects; failed effects cannot publish a validated output. Ordinary chat history,
provider continuation, and pending questions remain separate. Task identities must be
unused at first admission.

`SessionService::start_execution_graph` explicitly saves and starts a static graph in a
prepared session. Named provider-call limits remain shared across the launch, including
retries, repairs, and ordered utility children. Identical active launches do not refill
those limits. Acceptance uses `accept_execution_graph_result` with an exact validated
reference. Acceptance and Env release wake the active drivers; launch-time snapshots are
never discovered and replayed automatically at startup. Graph writers exclude ordinary
session work, merge workflows, and detached focused-review readers. Legacy focused
reviews retain their existing overlap with ordinary session work, including
conflict-free sync; they do not hold the ordinary session writer gate.

An empty dispatch result means no work was admitted, not that the graph is complete.
Hosts wake dispatch when gates or resource availability change. Graph cancellation,
explicit retry/resume, workflow migration, graph proposals, and runtime expansion remain
separate work. A dispatch failure stops that launch's driver. An identical explicit
launch restarts dispatch using its retained executor and remaining budgets; it does not
retry failed nodes or refill credits. Storage provides no automatic replay or permission
to revise started work.
