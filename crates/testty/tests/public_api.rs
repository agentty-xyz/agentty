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
    // Arrange, Act, Assert: compile-time references exercise the curated prelude
    // API.
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

    // Result-returning matcher core surface.
    let _: fn(&TerminalFrame, &str) -> MatchResult = assertion::match_not_visible;
    let _: Option<AssertionFailure> = None;
    let _: Option<Expected> = None;
}

/// Reference always-available stable items that live outside the prelude
/// but are documented as part of the public contract.
#[test]
fn auxiliary_surface_is_stable() {
    // Arrange, Act, Assert: compile-time references exercise documented auxiliary
    // APIs.
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
    // Arrange: construct public structs through their stable field names.
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

    // Act: read each public field so renames or removals break compilation.
    let region_fields = (region.col, region.row, region.width, region.height);
    let color_fields = (color.red, color.green, color.blue);

    // Assert: the stable field values are preserved.
    assert_eq!(region_fields, (0, 1, 2, 3));
    assert_eq!(color_fields, (10, 20, 30));
}

/// Lock in the supported pattern for matching `AssertionFailure` and the
/// `Expected` variants exposed by the `match_*` matcher core.
///
/// `AssertionFailure` and `Expected` are both `#[non_exhaustive]` so future
/// fields and variants stay non-breaking. Downstream callers must destructure
/// with named fields plus a trailing `..` rest-pattern and must include a
/// fallback `_` arm. This function is compiled (not run) so accidental
/// renames of variants or destructured field names fail the build before
/// publication. The bound values are referenced in each arm so clippy
/// keeps the compatibility check explicit instead of collapsing the named
/// fields into the trailing `..`.
#[allow(dead_code)]
fn assertion_failure_destructuring_is_stable(failure: &AssertionFailure) -> &'static str {
    let AssertionFailure {
        message,
        expected,
        region,
        matched_spans,
        frame_excerpt,
        ..
    } = failure;
    let _: (&String, &Option<Region>, &Vec<MatchedSpan>, &String) =
        (message, region, matched_spans, frame_excerpt);

    match expected {
        Expected::TextInRegion { needle, .. } => {
            let _: &String = needle;

            "text-in-region"
        }
        Expected::NotVisible { needle, .. } => {
            let _: &String = needle;

            "not-visible"
        }
        Expected::MatchCount { needle, count, .. } => {
            let _: (&String, &usize) = (needle, count);

            "match-count"
        }
        Expected::ForegroundColor { needle, color, .. } => {
            let _: (&String, &CellColor) = (needle, color);

            "foreground"
        }
        Expected::BackgroundColor { needle, color, .. } => {
            let _: (&String, &CellColor) = (needle, color);

            "background"
        }
        Expected::Highlighted { needle, .. } => {
            let _: &String = needle;

            "highlighted"
        }
        Expected::NotHighlighted { needle, .. } => {
            let _: &String = needle;

            "not-highlighted"
        }
        _ => "unknown",
    }
}

/// Lock in the supported pattern for matching `SnapshotError` variants.
///
/// `SnapshotError` and its struct-shaped variants are `#[non_exhaustive]`
/// so future variants and fields stay non-breaking. Downstream callers
/// must match with named fields plus a trailing `..` rest-pattern and
/// must include a fallback `_` arm. This function is compiled (not run)
/// so accidental renames of variants or destructured field names fail
/// the build before publication. The bound values are referenced in each
/// arm so clippy keeps the compatibility check explicit instead of
/// collapsing the named fields into the trailing `..`.
#[allow(dead_code)]
fn snapshot_error_destructuring_is_stable(error: &SnapshotError) -> &'static str {
    match error {
        SnapshotError::MissingBaseline {
            name,
            baseline_path,
            ..
        } => {
            let _: (&String, &PathBuf) = (name, baseline_path);

            "missing-baseline"
        }
        SnapshotError::Mismatch {
            name,
            diff_percent,
            threshold,
            baseline_path,
            actual_path,
            ..
        } => {
            let _: (&String, &f64, &f64, &PathBuf, &PathBuf) =
                (name, diff_percent, threshold, baseline_path, actual_path);

            "mismatch"
        }
        SnapshotError::FrameMismatch {
            name,
            expected,
            actual,
            ..
        } => {
            let _: (&String, &String, &String) = (name, expected, actual);

            "frame-mismatch"
        }
        SnapshotError::IoError(_message) => "io-error",
        SnapshotError::ImageError(_message) => "image-error",
        _ => "unknown",
    }
}
