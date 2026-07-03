+++
title = "Testability Boundaries"
description = "Trait boundaries around external systems and testing guidance for deterministic orchestration."
weight = 5
+++

<a id="architecture-testability-introduction"></a> Agentty keeps external systems behind
trait boundaries so orchestration logic can be tested deterministically.

<!-- more -->

## Testability and Boundaries

<a id="architecture-testability-boundaries"></a> External-boundary traits are mocked
with `mockall`, usually via `#[cfg_attr(test, mockall::automock)]`; shared workspace
crates such as `ag-forge` expose test mocks through crate features. The major
boundaries:

| Trait | Module | Boundary | |-------|--------|----------| | `GitClient` |
`infra/git/client.rs` | Git and worktree operations (merge, rebase, diff, push, status,
ahead/behind). | | `FsClient` | `infra/fs.rs` | Async filesystem operations and path
probes. | | `AgentChannel` | `infra/channel.rs` | Provider-agnostic turn execution. | |
`AgentBackend` | `infra/agent/backend.rs` | Per-provider setup and transport command
construction. | | `AppServerClient` | `infra/app_server/contract.rs` | Provider
app-server RPC execution and session runtime lifecycle. | | `ReviewRequestClient` |
`crates/ag-forge/src/client.rs` | GitHub/GitLab review-request orchestration through
`gh`/`glab`. | | `EventSource` | `runtime/event.rs` | Terminal event polling for
deterministic event-loop tests. | | `Clock` | `infra/clock.rs` | Wall-clock and
monotonic time for session orchestration and render throttling. | | `TmuxClient` |
`infra/tmux.rs` | Tmux subprocess operations for opening worktrees. | |
`ClipboardImageClient` | `infra/clipboard_image.rs` | Clipboard image capture and
temp-file persistence. | | Repository traits | `infra/db/*.rs` | Narrow persistence
boundaries (`SessionRepository`, `ProjectRepository`, `ReviewRepository`,
`UsageRepository`, `ActivityRepository`, `OperationRepository`, `SettingRepository`). |

Beyond these, narrower internal command-runner boundaries (for example
`ForgeCommandRunner`, `GitCommandRunner`, `TmuxCommandRunner`, `UpdateRunner`, and the
provider transport traits) keep subprocess sequencing and retry behavior deterministic
in unit tests. The runtime also accepts `Terminal<B: Backend>` via `run_with_backend`,
enabling in-process TUI tests with `TestBackend`.

## Typed Errors Across Layers

<a id="architecture-typed-error-enums"></a> Each infra boundary exposes a typed error
enum (`DbError`, `GitError`, `AppServerError`, `AgentError`, `ClipboardError`, and so
on) instead of opaque `String` errors. The conversion chain `AppServerTransportError` →
`AppServerError::Transport` → `AgentError::AppServer` allows `?`-propagation through the
transport, provider, and channel layers without collapsing causal context into formatted
strings.

<a id="architecture-app-layer-typed-errors"></a> The app layer propagates infra errors
through `SessionError` (`app/session/error.rs`) and `AppError` (`app/error.rs`), both of
which wrap the infra enums via `#[from]` plus a `Workflow(String)` variant for
contextual app-level failures. At event and display boundaries, errors are converted to
`String` via `Display` because those types require `Clone` and `Eq`.

## Testing Guidance

<a id="architecture-boundary-testing-guidance"></a> When adding higher-level flows
involving multiple external commands, prefer injectable trait boundaries and
`mockall`-based tests over flaky end-to-end shell-heavy tests. Add a narrower internal
command-runner boundary when a public orchestration trait still needs deterministic
coverage of subprocess sequencing or retry behavior.

Apply the same rule to filesystem discovery and path probes in `app/` and `runtime/`:
route directory walking, `exists` checks, `canonicalize`, and file copy or persistence
helpers through an infra boundary instead of calling `std::fs` or `Path` helpers
directly from orchestration code. Likewise, route `Instant::now()` and
`SystemTime::now()` through the shared `Clock` boundary.

## TUI E2E Testing Framework (`testty`)

<a id="architecture-tui-e2e-framework"></a> The `testty` workspace crate provides a
dual-oracle model for TUI end-to-end testing. The PTY path (`portable-pty` + `vt100`) is
the semantic oracle for text, style, and location assertions; the VHS path is the visual
oracle and review artifact generator.

| Module | Purpose | |--------|---------| | `session` | PTY executor: spawns binaries,
writes input, captures ANSI output. | | `frame` | Terminal frame parser: ANSI bytes to a
cell grid. | | `region` / `locator` | Rectangular regions and style-aware text locators.
| | `assertion` / `recipe` | Structured matchers and agent-friendly expectation helpers.
| | `scenario` / `step` / `journey` | Scenario DSL compiled to PTY or VHS. | | `vhs` /
`snapshot` / `proof` | VHS tape compilation, paired baselines, proof backends. | |
`feature` | `FeatureDemo` builder with hash-cached VHS GIF generation. |

testty has no crate-root re-export module: every public item is addressable only through
its owning module path (for example, `use testty::scenario::Scenario;`). The
`tests/public_api.rs` tripwire pins those per-module items as the documented stable
surface.
