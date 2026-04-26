//! Public-API compatibility tripwire for the `testty::prelude` surface.
//!
//! This test exists so that any accidental rename, removal, or signature
//! break of a curated stable item fails the build before publication.
//! Update it deliberately whenever an intentional breaking change is made
//! to the published surface, and bump the testty major version in lockstep.
//!
//! Coverage goes beyond symbol presence and exercises the source-compat
//! patterns we want to keep supporting:
//!
//! - struct-literal construction of types whose public field set is part of the
//!   stable contract (for example, [`Region`] and [`CellColor`]),
//! - pattern matching of [`SnapshotError`] variants through the supported `..`
//!   rest-pattern (so future field additions stay non-breaking),
//! - construction of [`SnapshotConfig`] through its public builder so the
//!   `#[non_exhaustive]` lock-down does not regress to struct-literal syntax in
//!   downstream code.

#![allow(unused_imports)]

use std::path::{Path, PathBuf};

use testty::prelude::*;
use testty::snapshot::{SnapshotConfig, SnapshotError};

/// Minimal `ProofBackend` impl used to exercise the trait through the
/// prelude alias without instantiating a real backend.
struct NoopBackend;

impl ProofBackend for NoopBackend {
    fn render(&self, _report: &ProofReport, _output: &Path) -> Result<(), ProofError> {
        Ok(())
    }
}

fn accept_backend<B: ProofBackend>(_backend: &B) {}

/// Reference every prelude type so the build breaks if a name is removed
/// or renamed in a backwards-incompatible way.
#[test]
fn prelude_surface_is_stable() {
    let _: Region = Region::new(0, 0, 1, 1);
    let _: CellColor = CellColor::new(0, 0, 0);
    let _: CellStyle = CellStyle::default();
    let _: fn(u16, u16, &[u8]) -> TerminalFrame = TerminalFrame::new;

    let _ = Scenario::new("public-api");
    let _: Option<Step> = None;
    let _: Option<Journey> = None;

    let _: Option<PtySessionBuilder> = None;
    let _: Option<PtySession> = None;
    let _: Option<PtySessionError> = None;

    let _: Option<MatchedSpan> = None;

    let _: Option<ProofCapture> = None;
    let _: Option<ProofReport> = None;
    let _: Option<ProofError> = None;

    let _: fn(&NoopBackend) = accept_backend::<NoopBackend>;

    let _: fn(&TerminalFrame, &str) = assertion::assert_not_visible;
}

/// Reference always-available stable items that live outside the prelude
/// but are documented as part of the public contract.
#[test]
fn auxiliary_surface_is_stable() {
    let _: &str = testty::snapshot::DEFAULT_UPDATE_ENV_VAR;

    let config = SnapshotConfig::new("/baselines", "/artifacts")
        .with_update_env_var("MY_VAR")
        .with_update_mode(true);
    let _: bool = config.is_update_mode();

    let _: fn(&TerminalFrame, &str) = testty::recipe::expect_instruction_visible;
}

/// Lock in struct-literal construction for prelude types whose public
/// field names are part of the stable contract. Renaming or removing any
/// of these fields will fail this test before publication.
#[test]
fn public_struct_literals_are_stable() {
    let region = Region {
        col: 0,
        row: 1,
        width: 2,
        height: 3,
    };
    assert_eq!(region.col, 0);
    assert_eq!(region.row, 1);
    assert_eq!(region.width, 2);
    assert_eq!(region.height, 3);

    let color = CellColor {
        red: 10,
        green: 20,
        blue: 30,
    };
    assert_eq!(color.red, 10);
    assert_eq!(color.green, 20);
    assert_eq!(color.blue, 30);
}

/// Lock in the supported pattern for matching `SnapshotError` variants.
///
/// `SnapshotError` and its struct-shaped variants are `#[non_exhaustive]`
/// so future variants and fields stay non-breaking. Downstream callers
/// must match with named fields plus a trailing `..` rest-pattern and
/// must include a fallback `_` arm. This function is compiled (not run)
/// so accidental renames of variants or destructured field names fail
/// the build before publication.
#[allow(dead_code)]
fn snapshot_error_destructuring_is_stable(error: &SnapshotError) -> &'static str {
    match error {
        SnapshotError::MissingBaseline {
            name: _,
            baseline_path: _,
            ..
        } => "missing-baseline",
        SnapshotError::Mismatch {
            name: _,
            diff_percent: _,
            threshold: _,
            baseline_path: _,
            actual_path: _,
            ..
        } => "mismatch",
        SnapshotError::FrameMismatch {
            name: _,
            expected: _,
            actual: _,
            ..
        } => "frame-mismatch",
        SnapshotError::IoError(_message) => "io-error",
        SnapshotError::ImageError(_message) => "image-error",
        _ => "unknown",
    }
}
