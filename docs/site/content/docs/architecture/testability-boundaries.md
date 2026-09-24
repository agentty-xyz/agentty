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

<!-- more -->

## Testability and Boundaries

Unit tests live in sibling `*_test.rs` modules. Shared fixtures remain test-only; public
integration tests exercise supported APIs without exposing private implementation.

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
| Workspace personalities          | `PersonalityCatalogClient`      |
| Process and temperature sampling | `ResourceClient`                |
| Persistence                      | Repository traits in `ag-store` |
| Storage timestamps               | `TimestampSource`               |

Use `mockall` mocks at the narrowest useful boundary. Smaller command-runner traits
isolate subprocess sequencing when a public client is too broad. Runtime rendering can
use an injected terminal backend. [Module Map](@/docs/architecture/module-map.md)
identifies the owning crates and layers.

Campaign tests inject repositories, `SessionBackend`, `OrchestrationEventSink`, and
`OrchestrationSchedule`. They do not construct the TUI. Storage tests exercise real
SQLite adapters with injected timestamps; test fixtures do not alter production
contracts.

Resource tests inject process identities and readings to cover PID reuse, stale data,
and sensor stalls. Host tests separately verify native access. A stalled sensor must not
block process accounting or shutdown.

Harness conformance tests exercise every supported store and executor through public
APIs. Store tests cover ownership, recovery, admission, and persistence; executor tests
cover capture, cancellation, cleanup, and confinement. Native enforcement requires real
platform qualification: mocks cannot prove it. Missing native infrastructure is a failed
qualification, not a passed or silently skipped check. See
[`ag-harness` Design](@/docs/architecture/ag-harness-design.md) for platform
limitations.

## Typed Errors Across Layers

<a id="architecture-typed-error-enums"></a> External clients expose typed errors.
Adapters translate transport details into shared execution error categories so workers
and hosts remain independent of provider implementations.

<a id="architecture-app-layer-typed-errors"></a> Application workflows propagate
`SessionError` and `AppError`. Convert them to display strings only at event and UI
boundaries; preserve causes until then.

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
