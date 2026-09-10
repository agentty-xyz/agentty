use std::path::Path;

use super::super::{GifContext, VhsContext, generate_gif};
use super::support::{failed_tape_execution, test_vhs_context, vhs_available};
use crate::feature::{GifMode, GifStatus, compute_gif_hash, hash_sidecar_path};
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::scenario::Scenario;
use crate::vhs::{VhsTape, VhsTapeSettings, check_vhs_installed};

#[test]
fn generate_gif_check_only_does_not_create_output_dir() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let missing_dir = temp.path().join("never_created");
    let report = ProofReport::new("check_only_readonly");
    let scenario = Scenario::new("check_only_readonly");
    let settings = VhsTapeSettings::feature_demo();
    let binary = Path::new("/usr/bin/true");
    let env_pairs: &[(&str, &str)] = &[];
    let vhs = VhsContext {
        binary_path: binary,
        check_vhs: check_vhs_installed,
        env_pairs,
        execute_tape: VhsTape::execute,
        settings: &settings,
    };

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        "check_only_readonly",
        &missing_dir,
        GifContext {
            mode: GifMode::CheckOnly,
            redactions: &[],
        },
        vhs,
    );

    // Assert — verdict is Stale with a missing sidecar and the output
    // directory is untouched.
    let GifStatus::Stale {
        committed,
        committed_error,
        ..
    } = status
    else {
        unreachable!("expected Stale verdict, got {status:?}");
    };

    assert!(committed.is_none());
    assert!(committed_error.is_none());
    assert!(
        !missing_dir.exists(),
        "CheckOnly must not create the output directory",
    );
}

#[test]
fn generate_gif_check_only_returns_fresh_when_gif_and_sidecar_match() {
    // Arrange — pre-stage a GIF file and a sidecar whose contents equal
    // the GIF hash for the report and render settings.
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "check_only_fresh";

    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);

    let gif_path = output_dir.join(format!("{name}.gif"));
    std::fs::write(&gif_path, b"fake-gif-bytes").expect("write fake gif");

    let sidecar = hash_sidecar_path(output_dir, name);
    std::fs::write(&sidecar, expected_hash.to_string()).expect("write sidecar");

    let scenario = Scenario::new(name);
    let binary = Path::new("/usr/bin/true");
    let env_pairs: &[(&str, &str)] = &[];
    let vhs = VhsContext {
        binary_path: binary,
        check_vhs: check_vhs_installed,
        env_pairs,
        execute_tape: VhsTape::execute,
        settings: &settings,
    };

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::CheckOnly,
            redactions: &[],
        },
        vhs,
    );

    // Assert — verdict is Fresh and exposes the GIF path plus the
    // computed hash.
    let GifStatus::Fresh {
        gif_path: returned_path,
        hash,
    } = status
    else {
        unreachable!("expected Fresh verdict, got {status:?}");
    };

    assert_eq!(returned_path, gif_path);
    assert_eq!(hash, expected_hash);
}

#[test]
fn generate_gif_check_only_reports_render_settings_change_as_stale() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "check_only_settings_change";
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);

    let mut previous_settings = VhsTapeSettings::feature_demo();
    previous_settings.width = 3200;
    previous_settings.height = 1600;
    previous_settings.font_size = 36;
    let committed_hash = compute_gif_hash(&report, &[], &previous_settings);
    let current_settings = VhsTapeSettings::feature_demo();
    let current_hash = compute_gif_hash(&report, &[], &current_settings);

    let gif_path = output_dir.join(format!("{name}.gif"));
    std::fs::write(&gif_path, b"previous-preset-gif").expect("write GIF");
    let sidecar = hash_sidecar_path(output_dir, name);
    std::fs::write(&sidecar, committed_hash.to_string()).expect("write sidecar");

    let scenario = Scenario::new(name);
    let vhs = test_vhs_context(&current_settings, vhs_available, failed_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::CheckOnly,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(
        status,
        GifStatus::Stale {
            current,
            committed: Some(committed),
            committed_error: None,
            ..
        } if current == current_hash
            && committed == committed_hash
            && current != committed
    ));
}

#[test]
fn generate_gif_check_only_reports_empty_gif_as_stale() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "check_only_empty";
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    std::fs::write(&gif_path, []).expect("write empty gif");
    std::fs::write(&hash_path, expected_hash.to_string()).expect("write sidecar");
    let scenario = Scenario::new(name);
    let vhs = test_vhs_context(&settings, vhs_available, failed_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::CheckOnly,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(
        status,
        GifStatus::Stale {
            gif_path: returned_path,
            committed: Some(committed),
            committed_error: None,
            ..
        } if returned_path == gif_path && committed == expected_hash
    ));
}

#[test]
fn generate_gif_check_only_reports_invalid_sidecar() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "check_only_invalid_sidecar";

    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);

    let gif_path = output_dir.join(format!("{name}.gif"));
    std::fs::write(&gif_path, b"fake-gif-bytes").expect("write fake gif");

    let sidecar = hash_sidecar_path(output_dir, name);
    std::fs::write(&sidecar, "not-a-number").expect("write invalid sidecar");

    let scenario = Scenario::new(name);
    let settings = VhsTapeSettings::feature_demo();
    let binary = Path::new("/usr/bin/true");
    let env_pairs: &[(&str, &str)] = &[];
    let vhs = VhsContext {
        binary_path: binary,
        check_vhs: check_vhs_installed,
        env_pairs,
        execute_tape: VhsTape::execute,
        settings: &settings,
    };

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::CheckOnly,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    let GifStatus::Stale {
        gif_path: returned_path,
        committed,
        committed_error,
        ..
    } = status
    else {
        unreachable!("expected Stale verdict, got {status:?}");
    };

    assert_eq!(returned_path, gif_path);
    assert!(committed.is_none());
    assert!(
        committed_error
            .as_deref()
            .is_some_and(|err| err.contains("failed to parse hash sidecar")),
        "expected parse error, got {committed_error:?}",
    );
}
