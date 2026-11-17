# Workspace Crates

`crates/` contains the Rust workspace members. Treat manifests and module roots as the
source of truth for the member and module inventory.

## Rust Conventions

### Workspace and Modules

- Member manifests inherit shared package metadata with `workspace = true`.
- Use singular Rust file names.
- A `module.rs` paired with `module/` is router-only: declarations and re-exports, with
  no runtime types, constants, functions, or implementations.

### Readability

- Order code for top-to-bottom reading: public before restricted before private, with
  callees ordered by first use.
- Prefer module-oriented internal imports, use direct item imports only when clearer,
  and do not mix imported-module and fully qualified styles.
- Add `new()` or `Default` only for meaningful initialization. Prefer associated
  constructors over free construction helpers.
- Put an inherent `impl` directly below its struct and trait implementations after it.
  Keep helpers used by one type inside that type's `impl`.
- Put public struct fields before private fields and alphabetize within each group.
- Use descriptive names; avoid near-identical names in one scope.
- Separate logical blocks with blank lines, including before explicit or implicit
  returns except in a one-expression block.
- Introduce abstractions only for reuse, reduced complexity, or testability. Inline
  pass-through wrappers that add no behavior or boundary.

### Tests and Boundaries

- Choose each test's level and fixture with `skills/test-pyramid/SKILL.md`; the rules
  below govern how test files are written.
- In `*_test.rs` files, explicitly import the modules, items, traits, and fixtures each
  file uses at file scope. Use `crate::` paths to the owning production module, or named
  `super::` imports for private items in the parent module. Import sibling fixtures
  through their support module. Test-module routers declare child modules; each child
  imports its own dependencies instead of relying on imports forwarded through the
  router. Preserve production visibility when importing private internals.
- Split large `*_test.rs` suites by behavior behind a test-module router; keep access to
  private internals through child modules rather than widening production visibility.
- Keep test-only imports, helpers, builders, and implementations in test files. Do not
  vary production fields, trait contracts, defaults, or control flow with test
  configuration; inject dependencies for deterministic tests. Mockable traits may retain
  `#[cfg_attr(test, mockall::automock)]` and feature-gated shared mocks.
- Keep suite-local fixtures with their tests, crate-wide fixtures in test-gated support
  modules, and cross-crate support behind `test-utils`. Put public-contract integration
  tests in each crate's `tests/`, shared integration helpers in `tests/support/`, and
  Agentty PTY workflows in `crates/agentty/tests/e2e/`.
- Keep coverage filename filters aligned with the test-file convention when moving
  suites; preserve coverage of production code.
- Keep a real test for an isolated external command. For flows with multiple external
  calls, inject a trait boundary and use deterministic mocks.
- Reuse named fixtures, builders, and expectation helpers instead of duplicating test
  setup. Do not expose production APIs solely to share test fixtures.
- Route process, filesystem, network, terminal, and time access through injected
  boundaries in orchestration code.
- When removing behavior, test the remaining supported path rather than only asserting
  that the old path is absent.

## Tokio

- Keep the codebase async; do not create a runtime merely to call `block_on()`.
- Enable only required Tokio features.
- Use `tokio::process::Command` for streamed subprocesses and
  `tokio::task::spawn_blocking` for blocking synchronous work.
- Prefer variable shadowing or a scoped block when cloning values into spawned `move`
  closures.
- Use `#[tokio::test]` for async tests and `tokio::time::sleep` for async delays.

### Mutex Selection

- Default to `std::sync::Mutex`; use `tokio::sync::Mutex` only when the protected
  critical section itself awaits.
- Never hold an async mutex merely to perform synchronous work such as writing to a
  `std::fs::File`.
