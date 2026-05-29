# testty: Remove `prelude`, document explicit imports

## Context

`crates/testty/src/prelude.rs` currently re-exports the common testty types as a
wildcard-importable surface. The crate's documentation (`README.md`,
`docs/getting-started.md`, `docs/scenarios.md`, `docs/journeys.md`,
`docs/proof-pipeline.md`, `docs/upgrading.md`) opens every example with
`use testty::prelude::*;`.

Real-world callers do not use the prelude:

- `crates/testty/examples/journey_composition.rs`,
  `crates/testty/examples/frame_diffing.rs`, and
  `crates/testty/examples/proof_pipeline.rs` use per-module imports.
- `crates/agentty/tests/e2e/**` uses per-module imports (`use testty::assertion;`,
  `use testty::scenario::Scenario;`, etc.).
- `skills/feature-test/SKILL.md` already documents per-module imports.

testty has no published consumers outside this workspace.

## Goal

Remove the wildcard prelude entirely and make per-module imports the only documented
import style.

## Non-goals

- Renaming or relocating existing modules.
- Adding new `pub use` re-exports at the crate root.
- Changing the recipe, snapshot, or feature APIs.

## Canonical import style

Every public item is reached through its owning module:

```rust
use testty::assertion::{self, MatchResult, SoftAssertions};
use testty::feature::{FeatureDemo, FeatureMeta, FeatureResult, GifMode, GifStatus};
use testty::frame::{CellColor, CellStyle, TerminalFrame};
use testty::journey::{Journey, StartupWait};
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::{PtySession, PtySessionBuilder, PtySessionError};
use testty::step::{FramePredicate, Step};
use testty::recipe;
```

Snippets only import the items they actually use.

## Code changes

1. Delete `crates/testty/src/prelude.rs`.
1. Update `crates/testty/src/lib.rs`:
   - Remove `pub mod prelude;`.
   - Remove the crate-level doc comment line that points at `[`prelude`]`.
1. Update `crates/testty/tests/public_api.rs`:
   - Drop `use testty::prelude::*;`.
   - Replace it with explicit per-module imports for every symbol the test currently
     pins, so the compile-time tripwire still locks every documented item to a known
     canonical path.

No source changes are required in `crates/agentty/tests/e2e/**`; confirm with a final
grep that no `use testty::prelude` reference survives.

## Documentation changes

4. `crates/testty/README.md`: rewrite each of the three code blocks that currently start
   with `use testty::prelude::*;` to use the explicit imports each block needs.
1. `crates/testty/docs/getting-started.md`, `crates/testty/docs/scenarios.md`,
   `crates/testty/docs/journeys.md`, `crates/testty/docs/proof-pipeline.md`: replace
   every `use testty::prelude::*;` with the minimal explicit imports for that snippet.
1. `crates/testty/docs/upgrading.md`: replace the existing "use prelude" migration note
   with a new entry that says the prelude has been removed and shows a small
   before/after demonstrating the explicit-import style.
1. `crates/testty/AGENTS.md` (and its symlinked `CLAUDE.md` / `GEMINI.md`):
   - Remove the bullet describing `src/prelude.rs` as the curated re-export set under
     "Entry Points".
   - Remove the rule under "Visibility Discipline" that requires re-exporting new
     user-facing types from `prelude.rs`.
   - Replace the removed rule with a new rule: "Public items are addressable only via
     their owning module — do not add crate-root re-exports."
   - Adjust the `tests/public_api.rs` bullet so it no longer mentions the prelude as the
     gating surface, but still describes the test as the canonical public-API tripwire.
1. `docs/site/content/docs/architecture/testability-boundaries.md`: update the line that
   currently references the `use testty::prelude::*;` re-export set so it describes the
   per-module import policy.

## Version + release policy

This change does not bump versions. testty has no consumers outside this workspace, so
the major-version rule in `crates/testty/AGENTS.md` is treated as protecting external
consumers that do not exist for this crate. The rule itself is preserved in `AGENTS.md`
so the policy applies if external consumers ever appear.

## Validation

- `prek run rustfmt-fix --files <touched .rs>`
- `prek run cargo-check --files <touched .rs>`
- `prek run test-testty-src --all-files --hook-stage manual`
- `prek run mdformat --files <touched .md>` and `prek run --files <touched .md>`
- Per-turn gate: `uv tool run prek run --all-files` and
  `cargo build --locked --all-targets --all-features`.

Final grep to confirm zero remaining references to `testty::prelude` and zero remaining
references to `prelude.rs` outside this spec.
