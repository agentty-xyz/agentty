use std::fs;

use super::pixel_distance;
use crate::snapshot::{
    SnapshotConfig, SnapshotError, assert_frame_snapshot_matches, assert_snapshot_matches,
};

#[test]
fn pixel_distance_identical_is_zero() {
    // Arrange
    let pixel = [100, 150, 200, 255];

    // Act
    let distance = pixel_distance(pixel, pixel);

    // Assert
    assert!(distance.abs() < f64::EPSILON);
}

#[test]
fn pixel_distance_opposite_colors() {
    // Arrange
    let pixel_a = [0, 0, 0, 255];
    let pixel_b = [255, 255, 255, 255];

    // Act
    let distance = pixel_distance(pixel_a, pixel_b);

    // Assert — sqrt(255^2 * 3) ≈ 441.67
    assert!(distance > 441.0);
    assert!(distance < 442.0);
}

#[test]
fn pixel_distance_ignores_alpha() {
    // Arrange
    let pixel_a = [100, 100, 100, 0];
    let pixel_b = [100, 100, 100, 255];

    // Act
    let distance = pixel_distance(pixel_a, pixel_b);

    // Assert
    assert!(distance.abs() < f64::EPSILON);
}

#[test]
fn frame_snapshot_returns_missing_baseline_error_outside_update_mode() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let config = SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));

    // Act
    let result = assert_frame_snapshot_matches(&config, "test", "Hello World");

    // Assert
    assert!(
        matches!(result, Err(SnapshotError::MissingBaseline { .. })),
        "expected MissingBaseline error, got {result:?}"
    );
}

#[test]
fn frame_snapshot_matches_identical_content() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let config = SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
    let baseline_path = config.baseline_dir.join("test_frame.txt");
    fs::create_dir_all(&config.baseline_dir).expect("failed to create baseline dir");
    fs::write(&baseline_path, "Hello World").expect("failed to write baseline");

    // Act
    let result = assert_frame_snapshot_matches(&config, "test", "Hello World");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn frame_snapshot_detects_mismatch() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let config = SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
    let baseline_path = config.baseline_dir.join("test_frame.txt");
    fs::create_dir_all(&config.baseline_dir).expect("failed to create baseline dir");
    fs::write(&baseline_path, "Hello World").expect("failed to write baseline");

    // Act
    let result = assert_frame_snapshot_matches(&config, "test", "Goodbye World");

    // Assert
    assert!(result.is_err());
}

#[test]
fn snapshot_config_with_custom_thresholds() {
    // Arrange / Act
    let config = SnapshotConfig::new("/baselines", "/artifacts").with_thresholds(50.0, 20.0);

    // Assert
    assert!((config.pixel_threshold - 50.0).abs() < f64::EPSILON);
    assert!((config.diff_percent_threshold - 20.0).abs() < f64::EPSILON);
}

#[test]
fn snapshot_config_default_update_env_var_is_tui_test_update() {
    // Arrange / Act
    let config = SnapshotConfig::new("/baselines", "/artifacts");

    // Assert
    assert_eq!(config.update_env_var, "TUI_TEST_UPDATE");
}

#[test]
fn snapshot_config_with_update_env_var_overrides_default() {
    // Arrange / Act
    let config =
        SnapshotConfig::new("/baselines", "/artifacts").with_update_env_var("MY_CUSTOM_VAR");

    // Assert
    assert_eq!(config.update_env_var, "MY_CUSTOM_VAR");
}

#[test]
fn snapshot_config_default_has_no_update_mode_override() {
    // Arrange / Act
    let config = SnapshotConfig::new("/baselines", "/artifacts");

    // Assert
    assert!(config.update_mode_override.is_none());
}

#[test]
fn snapshot_config_with_update_mode_sets_override() {
    // Arrange / Act
    let enabled = SnapshotConfig::new("/baselines", "/artifacts").with_update_mode(true);
    let disabled = SnapshotConfig::new("/baselines", "/artifacts").with_update_mode(false);

    // Assert
    assert_eq!(enabled.update_mode_override, Some(true));
    assert!(enabled.is_update_mode());
    assert_eq!(disabled.update_mode_override, Some(false));
    assert!(!disabled.is_update_mode());
}

#[test]
fn frame_snapshot_writes_baseline_when_update_mode_override_is_active() {
    // Arrange — drive update mode through the injected override so the
    // test never mutates process-global environment state.
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let config = SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"))
        .with_update_mode(true);
    let baseline_path = config.baseline_dir.join("custom_frame.txt");
    assert!(
        !baseline_path.exists(),
        "baseline must not exist before the test runs"
    );

    // Act
    let result = assert_frame_snapshot_matches(&config, "custom", "Hello custom env var");

    // Assert
    assert!(
        result.is_ok(),
        "update mode must succeed when the override is active, got {result:?}"
    );
    assert!(
        baseline_path.exists(),
        "expected baseline file to be written at {}",
        baseline_path.display()
    );
    let written = fs::read_to_string(&baseline_path).expect("failed to read baseline");
    assert_eq!(written, "Hello custom env var");
}

#[test]
fn assert_snapshot_matches_writes_baseline_when_update_mode_override_is_active() {
    // Arrange — write a tiny solid-color PNG that stands in for the
    // actual screenshot a caller would supply.
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let config = SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"))
        .with_update_mode(true);
    let actual_path = temp.path().join("actual.png");
    let actual_image = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
    actual_image
        .save(&actual_path)
        .expect("failed to write actual PNG");
    let baseline_path = config.baseline_dir.join("custom.png");
    assert!(
        !baseline_path.exists(),
        "baseline must not exist before the test runs"
    );

    // Act
    let result = assert_snapshot_matches(&config, "custom", &actual_path);

    // Assert
    assert!(
        result.is_ok(),
        "update mode must succeed when the override is active, got {result:?}"
    );
    assert!(
        baseline_path.exists(),
        "expected baseline PNG to be written at {}",
        baseline_path.display()
    );
}
