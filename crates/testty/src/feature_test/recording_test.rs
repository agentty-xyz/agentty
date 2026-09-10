use super::super::{GifContext, finalize_gif_recording, generate_gif};
use super::support::{
    GENERATED_GIF_BYTES, empty_tape_execution, failed_tape_execution, successful_tape_execution,
    test_vhs_context, vhs_available, vhs_unavailable,
};
use crate::feature::{GifMode, GifStatus, compute_gif_hash, hash_sidecar_path};
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::scenario::Scenario;
use crate::vhs::{VhsError, VhsTapeSettings};

#[test]
fn generate_gif_generate_if_stale_reuses_nonempty_cached_gif() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "cache_hit";
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    std::fs::write(&gif_path, GENERATED_GIF_BYTES).expect("write cached gif");
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
            mode: GifMode::GenerateIfStale,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(status, GifStatus::CacheHit(path) if path == gif_path));
    assert_eq!(
        std::fs::read(&gif_path).expect("read gif"),
        GENERATED_GIF_BYTES
    );
}

#[test]
fn generate_gif_generate_if_stale_replaces_empty_cached_gif() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "empty_cache";
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    std::fs::write(&gif_path, []).expect("write empty gif");
    std::fs::write(&hash_path, expected_hash.to_string()).expect("write sidecar");
    let scenario = Scenario::new(name).capture();
    let vhs = test_vhs_context(&settings, vhs_available, successful_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::GenerateIfStale,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(status, GifStatus::Generated(path) if path == gif_path));
    assert_eq!(
        std::fs::read(&gif_path).expect("read gif"),
        GENERATED_GIF_BYTES
    );
}

#[test]
fn generate_gif_success_invalidates_poster_and_cleans_recording_files() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "generated";
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new(name);
    report.add_capture("snap", "Snapshot", &frame);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    let poster_path = output_dir.join(format!("{name}.png"));
    let recording_path = output_dir.join(format!(".{name}.recording.gif"));
    let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
    let tape_path = output_dir.join(format!("{name}.tape"));
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);
    std::fs::write(&gif_path, b"previous gif").expect("write previous gif");
    std::fs::write(&poster_path, b"stale poster").expect("write poster");
    let scenario = Scenario::new(name).capture();
    let vhs = test_vhs_context(&settings, vhs_available, successful_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::AlwaysGenerate,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(status, GifStatus::Generated(path) if path == gif_path));
    assert_eq!(
        std::fs::read_to_string(&hash_path).expect("read hash"),
        format!("{expected_hash}\n")
    );
    assert_eq!(
        std::fs::read(&gif_path).expect("read gif"),
        GENERATED_GIF_BYTES
    );
    assert!(!poster_path.exists());
    assert!(!recording_path.exists());
    assert!(!screenshot_path.exists());
    assert!(!tape_path.exists());
}

#[test]
fn generate_gif_failure_preserves_artifacts_and_cleans_recording_files() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "failed";
    let report = ProofReport::new(name);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    let poster_path = output_dir.join(format!("{name}.png"));
    let recording_path = output_dir.join(format!(".{name}.recording.gif"));
    let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
    let tape_path = output_dir.join(format!("{name}.tape"));
    std::fs::write(&gif_path, b"valid gif").expect("write gif");
    std::fs::write(&hash_path, b"previous hash\n").expect("write hash");
    std::fs::write(&poster_path, b"valid poster").expect("write poster");
    let scenario = Scenario::new(name).capture();
    let settings = VhsTapeSettings::feature_demo();
    let vhs = test_vhs_context(&settings, vhs_available, failed_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::AlwaysGenerate,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(status, GifStatus::TapeExecutionFailed(_)));
    assert_eq!(std::fs::read(&gif_path).expect("read gif"), b"valid gif");
    assert_eq!(
        std::fs::read(&hash_path).expect("read hash"),
        b"previous hash\n"
    );
    assert_eq!(
        std::fs::read(&poster_path).expect("read poster"),
        b"valid poster"
    );
    assert!(!recording_path.exists());
    assert!(!screenshot_path.exists());
    assert!(!tape_path.exists());
}

#[test]
fn generate_gif_empty_recording_preserves_existing_artifacts() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "empty_recording";
    let report = ProofReport::new(name);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let hash_path = hash_sidecar_path(output_dir, name);
    let poster_path = output_dir.join(format!("{name}.png"));
    let recording_path = output_dir.join(format!(".{name}.recording.gif"));
    let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
    let tape_path = output_dir.join(format!("{name}.tape"));
    std::fs::write(&gif_path, b"valid gif").expect("write gif");
    std::fs::write(&hash_path, b"previous hash\n").expect("write hash");
    std::fs::write(&poster_path, b"valid poster").expect("write poster");
    let scenario = Scenario::new(name).capture();
    let settings = VhsTapeSettings::feature_demo();
    let vhs = test_vhs_context(&settings, vhs_available, empty_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::AlwaysGenerate,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(
        status,
        GifStatus::TapeExecutionFailed(err)
            if err.to_string().contains("did not produce a nonempty GIF")
    ));
    assert_eq!(std::fs::read(&gif_path).expect("read gif"), b"valid gif");
    assert_eq!(
        std::fs::read(&hash_path).expect("read hash"),
        b"previous hash\n"
    );
    assert_eq!(
        std::fs::read(&poster_path).expect("read poster"),
        b"valid poster"
    );
    assert!(!recording_path.exists());
    assert!(!screenshot_path.exists());
    assert!(!tape_path.exists());
}

#[test]
fn finalize_gif_recording_reports_replacement_failure() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let recording_path = temp.path().join("recording.gif");
    let gif_path = temp.path().join("feature.gif");
    std::fs::write(&recording_path, GENERATED_GIF_BYTES).expect("write recording");
    std::fs::create_dir(&gif_path).expect("create conflicting GIF directory");

    // Act
    let result = finalize_gif_recording(&recording_path, &gif_path);

    // Assert
    assert!(matches!(result, Err(VhsError::IoError(_))));
    assert!(recording_path.exists());
    assert!(gif_path.is_dir());
}

#[test]
fn generate_gif_missing_vhs_preserves_artifacts() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let output_dir = temp.path();
    let name = "missing_vhs";
    let report = ProofReport::new(name);
    let poster_path = output_dir.join(format!("{name}.png"));
    std::fs::write(&poster_path, b"valid poster").expect("write poster");
    let scenario = Scenario::new(name);
    let settings = VhsTapeSettings::feature_demo();
    let vhs = test_vhs_context(&settings, vhs_unavailable, failed_tape_execution);

    // Act
    let status = generate_gif(
        &scenario,
        &report,
        name,
        output_dir,
        GifContext {
            mode: GifMode::GenerateIfStale,
            redactions: &[],
        },
        vhs,
    );

    // Assert
    assert!(matches!(status, GifStatus::VhsNotInstalled));
    assert!(poster_path.exists());
}
