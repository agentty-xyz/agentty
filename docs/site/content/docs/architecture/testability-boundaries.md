+++
title = "Testability Boundaries"
description = "External-system boundaries and deterministic testing guidance."
weight = 5
+++

<a id="architecture-testability-introduction"></a> Inject external systems so workflow
tests can control failures, ordering, and time without live providers or host-dependent
results. Use real adapter tests to verify the boundaries themselves.

<!-- more -->

## Testability and Boundaries

Unit tests live in sibling `*_test.rs` modules. Shared fixtures remain test-only; public
integration tests exercise supported APIs without exposing private implementation.

<a id="architecture-testability-boundaries"></a> Major injectable contracts:

| Boundary                       | Contract                        |
| ------------------------------ | ------------------------------- |
| Git and worktrees              | `GitClient`                     |
| Filesystem and path probes     | `FsClient`                      |
| Session turns                  | `AgentChannel`                  |
| Isolated model calls           | `OneShotClient`                 |
| Provider setup                 | `AgentBackend`                  |
| Provider runtime lifecycle     | `AppServerClient`               |
| Provider availability, sign-in | `AgentAvailabilityProbe`        |
| Forge requests and comments    | `ReviewRequestClient`           |
| Programmatic session lifecycle | `SessionBackend`                |
| Terminal events                | `EventSource`                   |
| Wall and monotonic time        | `Clock`                         |
| Worktree launch commands       | `TmuxClient`                    |
| Clipboard capture              | `ClipboardImageClient`          |
| Terminal clipboard copies      | `TerminalClipboard`             |
| Browser pages and links        | `BrowserClient`                 |
| Workspace agents               | `AgentCatalogClient`            |
| Process sampling               | `ResourceClient`                |
| Remote-control listener        | `RemoteControlLauncher`         |
| Paired remote devices          | `DeviceStore`                   |
| Persistence                    | Repository traits in `ag-store` |
| Storage timestamps             | `TimestampSource`               |

Use `mockall` mocks at the narrowest useful boundary. Smaller command-runner traits
isolate subprocess sequencing when a public client is too broad. Runtime rendering can
use an injected terminal backend. [Module Map](@/docs/architecture/module-map.md)
identifies the owning crates and layers.

## What Each Boundary Proves

- **Pure contracts** such as execution-graph validation, proposal resolution, and status
  policies are tested without I/O.
- **Storage** tests use real SQLite adapters with injected timestamps. They prove atomic
  admission, rollback, ownership, idempotent redelivery, and that reopening the database
  restores state without replaying or dispatching work.
- **Workers** are tested through injected queues, hosts, persistence, and runtime
  contracts; scripted providers stand in for models. Real process tests verify
  descendant cleanup and inherited pipes.
- **Application workflows** receive scripted clients through `SessionRunFactory` or
  `MockRunClient`, so ordering, cancellation, retries, and stale-result rejection are
  deterministic.
- **Providers and transports** are tested against local HTTP servers or injected
  provider events. Live provider behavior, event availability, and native sandbox
  enforcement need separate qualification; mocks cannot prove them, and missing native
  infrastructure is a failed qualification, not a skipped check.
- **PTY scenarios** cover user journeys end to end with pinned time and scripted
  providers.

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
- Keep explicit expectations in model catalog, default, and retirement tests; other
  workflows use the pinned selections from the `ag-session` `test-utils` feature.

## TUI E2E Testing Framework (`testty`)

<a id="architecture-tui-e2e-framework"></a> PTY tests provide semantic assertions over
terminal text, style, and position. VHS recordings provide visual review artifacts. Pin
time, provider executables, and version labels so recordings do not depend on the host.
PTY scenarios can advance monotonic time through the pinned `Clock` adapter; wall time
stays fixed for captures.

Follow `crates/agentty/tests/e2e/AGENTS.md` for PTY scenarios and
`skills/refresh-feature-recordings/SKILL.md` for published recordings.

## Headless worker boundaries

Inject `WorkQueue`, `WorkerHost`, persistence, runtime contracts, and the worker clock.
Cover shared ordering, heartbeats, cancellation, shutdown, and terminal settlement
without a frontend.

## Session composition boundary

Application tests receive scripted worker clients and provider workspace setup through
`SessionRunFactory`. The default test factory is offline, skips provider CLI setup, and
fails unexpected model calls, including detached ones.

## Worker submission boundary

Utility workflows inject `MockRunClient`; session workflows use scripted
`SessionRunClient` instances. Worker tests own raw runtime and storage injection.
Cancellation tests must observe adapter cleanup before terminal persistence, including
nested calls, caller drop, and provider panics. See
[Execution](@/docs/core-components/execution-components/execution-boundary.md) for the
mandatory worker path.

## Prompt Behavior

Contract tests cover schemas, native policy delivery, evidence encoding, and
continuation invalidation. Behavioral fixtures run through the production worker and
runtime with an injected provider. Opt-in live evaluations use the same path and record
responses, settings, usage, latency, and bounded grades. Grades are regression signals,
not a proof of correctness.

See `skills/evaluate-prompts/SKILL.md` for the evaluation workflow and root `AGENTS.md`
for required gates.

## Trace Export

Hosts compose OTLP export from the `ag-telemetry` `otlp` feature and choose the
destination; execution crates use its span guards with default features disabled. Local
HTTP receivers verify protobuf payloads and exporter failure, while in-memory exporters
verify span ownership and asynchronous context. `git.command` spans are verified against
real isolated `git` processes; `forge.command` spans use mocked forge command runners.
Trace boundaries report observable backend activity without inferring hidden model or
tool timings.
