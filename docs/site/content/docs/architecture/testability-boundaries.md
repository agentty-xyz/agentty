+++
title = "Testability Boundaries"
description = "External-system boundaries and deterministic testing guidance."
weight = 5
+++

<a id="architecture-testability-introduction"></a> Inject external systems so workflow
tests can control failures, ordering, and time without live providers or host-dependent
results. Use real adapter tests to verify the boundaries themselves.

Worker policy tests observe resolved `ExecutionPolicy` values at injected runtime
boundaries. Adapter tests verify native command settings, unsupported-policy errors,
repair propagation, and process-reuse compatibility without live model backends. These
tests establish policy delivery; actual provider enforcement still depends on the
installed harness and its supported controls.

Activity tests inject provider JSON and worker events to cover lifecycle deduplication,
retry identity, bounded input/output previews, utility forwarding, and transcript
persistence without live providers. Transport fixtures cover Gemini and Antigravity
activity forwarding. A `FeatureTest` PTY scenario covers live command/results and saved
usage footers. Live backend event availability still depends on the installed CLI
version.

<!-- more -->

## Testability and Boundaries

Unit tests live in sibling `*_test.rs` modules. Shared fixtures remain test-only; public
integration tests exercise supported APIs without exposing private implementation.

Execution graph validation and result binding are pure `ag-session` contracts. Unit
tests cover structural errors, profile restrictions, and stale or mismatched result
references. Public-contract tests describe plan acceptance and fresh review branches
with a join. `GraphRepository` boundary tests cover immutable revisions, ownership,
exact input binding, result evidence, and transactional rollback. A public on-disk test
retains accepted results and execution receipts across reopening without retargeting or
replay. These checks do not establish graph dispatch.

The `ag-session` `test-utils` feature supplies pinned model selections for unrelated
workflow tests and demos. Model catalog, default, and retirement tests keep independent,
explicit expectations so fixture reuse does not hide changes to those contracts.

<a id="architecture-testability-boundaries"></a> Major injectable contracts:

| Boundary                         | Contract                        |
| -------------------------------- | ------------------------------- |
| Git and worktrees                | `GitClient`                     |
| Filesystem and path probes       | `FsClient`                      |
| Session turns                    | `AgentChannel`                  |
| Isolated model calls             | `OneShotClient`                 |
| Provider setup                   | `AgentBackend`                  |
| Provider runtime lifecycle       | `AppServerClient`               |
| Forge requests and comments      | `ReviewRequestClient`           |
| Programmatic session lifecycle   | `SessionBackend`                |
| Terminal events                  | `EventSource`                   |
| Wall and monotonic time          | `Clock`                         |
| Worktree launch commands         | `TmuxClient`                    |
| Clipboard capture                | `ClipboardImageClient`          |
| Workspace agents                 | `AgentCatalogClient`            |
| Process and temperature sampling | `ResourceClient`                |
| Persistence                      | Repository traits in `ag-store` |
| Storage timestamps               | `TimestampSource`               |

`GitClient` keeps committed-tree publication evidence separate from worktree diffs. It
acquires advertised branch commits independently of fetch refspecs without changing
tracking refs or `FETCH_HEAD`; contract tests include restricted single-branch clones.
Public Git contract tests cover unpublished local commits and staged, unstaged, and
untracked edits; description preservation tests protect Markdown blocks and whitespace.

`TaskRepository` supports independent task creation, session-scoped snapshot listing,
and atomic admission of tasks, attempts, and operations. Attempt status comes from
operations. In-memory SQLite contract tests cover planning without execution, identity
conflicts, rollback, cancellation, and restart settlement. Scripted worker tests cover
the task API's handoff and completion timing without live providers.

Workflow replies are stored with their transcript prompt in one transaction, keyed by
the caller's reply identity. Boundary tests cover identity conflicts and duplicate
delivery, and an on-disk contract test keeps the receipt after reopening. Orchestrator
decisions are tested with a mock `RunClient`.

Role-aware task admission also binds immutable role/model selections and task-owned
conversation identities in the same transaction. Cached SQLite boundary tests cover
isolation from ordinary task delivery, immutable inputs, and rollback. A public on-disk
contract test verifies identity and delivery checks after reopening. The injected agent
catalog combines bundled defaults with repository definitions. Parser tests cover
supported fields and rejected execution settings; filesystem tests cover override
precedence, disabled entries, and containment. Task storage retains definition snapshots
independently of subsequent catalog changes.

Task-turn persistence commits response history, provider continuation, and usage in one
transaction without replacing ordinary conversation state. Boundary tests cover
cancellation, duplicate and conflicting saves, cross-task isolation, and rollback after
a usage-write failure. Reopening tests prove durable continuation through the public
repository API.

Plan acceptance uses a versioned ledger independent of task completion. Repository tests
prove exact plan handoff, latest-version checks, ownership and captured-selection
validation, and transactional rejection before Implementer admission.

Static workflow storage atomically reserves role tasks, accepts exact plan versions, and
admits attempts. Its phase is projected from operation-backed attempts rather than
stored as a second lifecycle. Pure tests cover phase projection; SQLite boundary tests
cover historical delivery receipts, cancellation scope, and rollback of failed handoffs.
A public reopening contract proves interrupted implementation remains failed until an
explicit retry.

Continuation receipts bind each delivery to its observed source attempt. Transactional
tests cover stale guidance after a role change, changed anchors on repeated deliveries,
cross-action identity reuse, and rollback when the receipt cannot be saved.

Use `mockall` mocks at the narrowest useful boundary. Smaller command-runner traits
isolate subprocess sequencing when a public client is too broad. Runtime rendering can
use an injected terminal backend. [Module Map](@/docs/architecture/module-map.md)
identifies the owning crates and layers.

Storage tests exercise real SQLite adapters with injected timestamps; test fixtures do
not alter production contracts. Project catalog visibility loads in one settings batch,
independent of catalog size. Session-list archive windows are bounded by the repository
query; test pagination with real SQLite and selection behavior through the host and PTY.

Resource tests inject process identities and readings to cover PID reuse, stale data,
and sensor stalls. Host tests separately verify native access. A stalled sensor must not
block process accounting or shutdown.

PTY expiration scenarios can advance monotonic time through the pinned `Clock` adapter
after observing the state whose deadline they exercise. Wall time stays fixed for
captures; monotonic advances never rewind and retain normal host-clock progress.
Production clocks and timeout durations keep their normal defaults.

Harness conformance tests exercise every supported store and executor through public
APIs. Store tests cover ownership, recovery, admission, and persistence; executor tests
cover capture, cancellation, cleanup, and confinement. Native enforcement requires real
platform qualification: mocks cannot prove it. Missing native infrastructure is a failed
qualification, not a passed or silently skipped check. See
[`ag-harness` Design](@/docs/architecture/ag-harness-design.md) for platform
limitations.

`ag-router` tests use local HTTP servers to verify provider payloads, bounded transport,
response decoding, and schema validation. `ag-harness` provider tests exercise its
public adapter into the router, including tool-call translation and telemetry; live
provider qualification remains separate. `ag-agent` native-harness tests inject the
environment through `NativeHarnessConfig::with_environment` and drive session turns,
utilities, cancellation, and `bash` against a scripted Chat Completions server; PTY
scenarios run Harness sessions against the same kind of server.

Tool-trace tests feed deterministic provider events to verify metadata, opt-in content,
credential suppression, and preview limits. Public trace-context tests cover capture
policy propagation; PTY scenarios inspect real OTLP payloads with capture enabled and
disabled. Provider fields absent from these supported event shapes remain a live backend
qualification gap, rather than inferred telemetry.

Public repository tests verify that unfinished forge review targets survive reopening
the database, remain scoped to their open review request, and clear with the completed
forge operation. Worker and forge mocks cover clarification continuation, serialized
post-push effects, manual retries, and stale-commit rejection.

Focused review persistence is checked against its active invocation in SQLite. Store
tests exercise the write and invalidation boundary with the real adapter, including
atomic archival and retrieval of completed discovery and consolidation call records.

Project review rules use bounded filesystem reads before parsing. Adapter tests verify
the read cap and I/O failures; workflow tests verify size-limit rejection before model
work.

## Typed Errors Across Layers

<a id="architecture-typed-error-enums"></a> External clients expose typed errors.
Adapters translate transport details into shared execution error categories so workers
and hosts remain independent of provider implementations.

<a id="architecture-app-layer-typed-errors"></a> Application workflows propagate
`SessionError` and `AppError`. Convert them to display strings only at event and UI
boundaries; preserve causes until then.

Project discovery is an injected capability invoked only after explicit user consent.
Startup catalog loads are database-only. Tests assert zero discovery calls before
consent, selected-root scope, cancellation, stale completions, and explicit result
registration; PTY scenarios cover the invitation and project controls. Operating-system
permission prompts require separate macOS verification.

## Testing Guidance

<a id="architecture-boundary-testing-guidance"></a>

- Inject process, filesystem, and clock access in application and runtime workflows.
- Test ordering, failure, cancellation, and retries through controllable boundaries.
- Use isolated real-system tests for behavior mocks cannot establish.
- Keep executable selection, canonical path validation, bounded output, and inherited
  process configuration handling inside command adapters.
- Keep production defaults intact; tests select offline clients through composition.

## TUI E2E Testing Framework (`testty`)

<a id="architecture-tui-e2e-framework"></a> PTY tests provide semantic assertions over
terminal text, style, and position. VHS recordings provide visual review artifacts. Pin
time, provider executables, and version labels so recordings do not depend on the host.
Import public `testty` items through their owning modules.

Follow `crates/agentty/tests/e2e/AGENTS.md` for PTY scenarios and
`docs/contributing/feature-test/recording.md` for published recordings.

## Headless worker boundaries

Inject `WorkQueue`, `WorkerHost`, persistence, runtime contracts, and the worker clock.
Cover shared ordering, heartbeats, cancellation, shutdown, and terminal settlement
without a frontend. Real process tests verify descendant cleanup and inherited pipes.

## Session composition boundary

Application tests receive scripted worker clients through `SessionRunFactory`. The
default test factory is offline and fails unexpected model calls, including detached
ones. Exercise model-switch scheduling and persistence failure through the production
composition path, preserving pending work when a save fails.

## Worker submission boundary

Utility workflows inject `MockRunClient`; session workflows use scripted
`SessionRunClient` instances. Worker tests own raw runtime and storage injection.
Preserve public submission-contract coverage alongside workflow tests.

Cancellation tests must observe adapter cleanup before terminal persistence, including
nested calls, caller drop, and provider panics. Review tests control deadlines and
request generations to verify partial retry, invalidation, and stale-result rejection. A
cached successful call is reusable only for the same evidence and profile.

See [Execution](@/docs/core-components/execution.md) for the mandatory worker path.

## Prompt Behavior

Contract tests cover schemas, native policy delivery, evidence encoding, and
continuation invalidation. Behavioral fixtures run through the production worker and
runtime with an injected provider. Opt-in live evaluations use the same path and record
responses, settings, usage, latency, and bounded grades. Grades are regression signals,
not a proof of correctness; unavailable telemetry remains unknown.

See `docs/contributing/prompts.md` for the evaluation workflow and root `AGENTS.md` for
required gates.

## Trace Export

Hosts compose OTLP export from the `ag-telemetry` `otlp` feature and choose the
destination; execution crates use its span guards with default features disabled. Local
HTTP receivers verify protobuf payloads and exporter failure, while in-memory exporters
verify span ownership and asynchronous context. Trace boundaries report observable
backend activity without inferring hidden model or tool timings.

Validation coordination injects metadata, process, fingerprint, ledger, and lock
boundaries. Public CLI tests use stub executables; a native lock test verifies exclusive
access and release. Provider event fixtures verify cumulative usage, duplicate updates,
unknown counters, compaction, and phase ownership. These checks do not measure live
provider latency or establish message quality for omitted diff evidence.
