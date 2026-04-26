//! Paired snapshot workflow for baseline management and failure artifacts.
//!
//! Manages committed baseline snapshots (visual PNG plus semantic frame
//! sidecar) and produces structured diffs on failure. Supports an
//! environment-driven update mode so baselines are only rewritten when
//! the author explicitly requests it.

use std::path::{Path, PathBuf};
use std::{env, fs};

use image::GenericImageView;

/// Default environment variable name that enables baseline update mode.
pub const DEFAULT_UPDATE_ENV_VAR: &str = "TUI_TEST_UPDATE";

/// Default pixel color-distance threshold for image comparison.
const DEFAULT_PIXEL_THRESHOLD: f64 = 30.0;

/// Default percentage of pixels allowed to differ.
const DEFAULT_DIFF_PERCENT_THRESHOLD: f64 = 10.0;

/// Configuration for snapshot comparison behavior.
///
/// Marked `#[non_exhaustive]` so future field additions are non-breaking
/// for downstream callers. Construct instances via [`SnapshotConfig::new`]
/// and the `with_*` builder methods rather than struct literal syntax.
#[derive(Debug, Clone)]
#[must_use]
#[non_exhaustive]
pub struct SnapshotConfig {
    /// Directory where reference baselines are stored.
    pub baseline_dir: PathBuf,
    /// Directory where failure artifacts are written.
    pub artifact_dir: PathBuf,
    /// Per-pixel color distance threshold.
    pub pixel_threshold: f64,
    /// Maximum percentage of differing pixels allowed.
    pub diff_percent_threshold: f64,
    /// Environment variable name that triggers baseline update mode when set.
    /// Defaults to [`DEFAULT_UPDATE_ENV_VAR`] (`TUI_TEST_UPDATE`). Set via
    /// [`SnapshotConfig::with_update_env_var`].
    update_env_var: String,
    /// Programmatic override for update mode. When `Some`, replaces the
    /// environment-variable check entirely. Set via
    /// [`SnapshotConfig::with_update_mode`] so tests and programmatic callers
    /// drive update mode without mutating process-global environment state.
    update_mode_override: Option<bool>,
}

impl SnapshotConfig {
    /// Create a new snapshot config with the given baseline and artifact
    /// directories, using default tolerance thresholds and the default
    /// `TUI_TEST_UPDATE` environment variable name.
    pub fn new(baseline_dir: impl Into<PathBuf>, artifact_dir: impl Into<PathBuf>) -> Self {
        Self {
            baseline_dir: baseline_dir.into(),
            artifact_dir: artifact_dir.into(),
            pixel_threshold: DEFAULT_PIXEL_THRESHOLD,
            diff_percent_threshold: DEFAULT_DIFF_PERCENT_THRESHOLD,
            update_env_var: DEFAULT_UPDATE_ENV_VAR.to_string(),
            update_mode_override: None,
        }
    }

    /// Set custom tolerance thresholds.
    pub fn with_thresholds(mut self, pixel_threshold: f64, diff_percent_threshold: f64) -> Self {
        self.pixel_threshold = pixel_threshold;
        self.diff_percent_threshold = diff_percent_threshold;

        self
    }

    /// Override the environment variable name that triggers update mode.
    pub fn with_update_env_var(mut self, var_name: impl Into<String>) -> Self {
        self.update_env_var = var_name.into();

        self
    }

    /// Force update mode on or off, bypassing the environment variable check.
    ///
    /// Provides an injected boundary so tests and programmatic callers can
    /// drive baseline-update behavior without mutating process-global
    /// environment state. Calling this with `true` makes [`is_update_mode`]
    /// return `true`; calling it with `false` forces it to return `false`
    /// even when the configured env var is set.
    ///
    /// [`is_update_mode`]: Self::is_update_mode
    pub fn with_update_mode(mut self, active: bool) -> Self {
        self.update_mode_override = Some(active);

        self
    }

    /// Check whether update mode is active for this config.
    ///
    /// The programmatic override (set via [`with_update_mode`]) takes
    /// precedence; when no override is set, the configured environment
    /// variable is consulted.
    ///
    /// [`with_update_mode`]: Self::with_update_mode
    #[must_use]
    pub fn is_update_mode(&self) -> bool {
        if let Some(forced) = self.update_mode_override {
            return forced;
        }

        env::var(&self.update_env_var).is_ok()
    }
}

/// Compare an actual screenshot against its stored baseline.
///
/// In update mode, saves the actual as the new baseline. When not in
/// update mode a committed baseline must already exist — a missing
/// baseline is treated as an error so CI never silently passes without
/// a reference.
///
/// # Errors
///
/// Returns an error if the baseline is missing (outside update mode),
/// the images differ beyond tolerance, or if I/O fails.
pub fn assert_snapshot_matches(
    config: &SnapshotConfig,
    name: &str,
    actual_screenshot: &Path,
) -> Result<(), SnapshotError> {
    fs::create_dir_all(&config.baseline_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;
    fs::create_dir_all(&config.artifact_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    let baseline_path = config.baseline_dir.join(format!("{name}.png"));

    if config.is_update_mode() {
        fs::copy(actual_screenshot, &baseline_path)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Ok(());
    }

    if !baseline_path.exists() {
        return Err(SnapshotError::MissingBaseline {
            name: name.to_string(),
            baseline_path,
            update_env_var: config.update_env_var.clone(),
        });
    }

    let diff_percent =
        compare_screenshots(actual_screenshot, &baseline_path, config.pixel_threshold)?;

    if diff_percent > config.diff_percent_threshold {
        let actual_artifact = config.artifact_dir.join(format!("{name}_actual.png"));
        fs::copy(actual_screenshot, &actual_artifact)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Err(SnapshotError::Mismatch {
            name: name.to_string(),
            diff_percent,
            threshold: config.diff_percent_threshold,
            baseline_path,
            actual_path: actual_artifact,
        });
    }

    Ok(())
}

/// Compare a frame text dump against its stored semantic baseline.
///
/// In update mode, saves the actual dump as the new baseline. When not
/// in update mode a committed baseline must already exist — a missing
/// baseline is treated as an error so CI never silently passes without
/// a reference.
///
/// # Errors
///
/// Returns an error if the baseline is missing (outside update mode),
/// the text differs, or if I/O fails.
pub fn assert_frame_snapshot_matches(
    config: &SnapshotConfig,
    name: &str,
    actual_text: &str,
) -> Result<(), SnapshotError> {
    fs::create_dir_all(&config.baseline_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;
    fs::create_dir_all(&config.artifact_dir)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    let baseline_path = config.baseline_dir.join(format!("{name}_frame.txt"));

    if config.is_update_mode() {
        fs::write(&baseline_path, actual_text)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Ok(());
    }

    if !baseline_path.exists() {
        return Err(SnapshotError::MissingBaseline {
            name: name.to_string(),
            baseline_path,
            update_env_var: config.update_env_var.clone(),
        });
    }

    let expected_text = fs::read_to_string(&baseline_path)
        .map_err(|err| SnapshotError::IoError(err.to_string()))?;

    if actual_text != expected_text {
        let actual_artifact = config.artifact_dir.join(format!("{name}_frame_actual.txt"));
        fs::write(&actual_artifact, actual_text)
            .map_err(|err| SnapshotError::IoError(err.to_string()))?;

        return Err(SnapshotError::FrameMismatch {
            name: name.to_string(),
            expected: expected_text,
            actual: actual_text.to_string(),
        });
    }

    Ok(())
}

/// Errors from snapshot comparison operations.
///
/// Both the enum and the struct-shaped variants are marked
/// `#[non_exhaustive]` so future variant or field additions are non-breaking
/// for downstream callers. Match arms must include a fallback `_` and any
/// destructuring of struct-shaped variants must use `..`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Screenshot pixel comparison exceeded threshold.
    #[error(
        "Snapshot '{name}' mismatch: {diff_percent:.1}% pixels differ \
         (threshold: {threshold}%).\nBaseline: {}\nActual: {}",
        baseline_path.display(),
        actual_path.display()
    )]
    #[non_exhaustive]
    Mismatch {
        /// Name of the snapshot.
        name: String,
        /// Percentage of differing pixels.
        diff_percent: f64,
        /// Configured threshold.
        threshold: f64,
        /// Path to the baseline file.
        baseline_path: PathBuf,
        /// Path to the saved actual file.
        actual_path: PathBuf,
    },

    /// No committed baseline file found (outside update mode).
    #[error(
        "Missing baseline for '{name}'. Run with {update_env_var}=1 to create it.\n\
         Expected: {}",
        baseline_path.display()
    )]
    #[non_exhaustive]
    MissingBaseline {
        /// Name of the snapshot.
        name: String,
        /// Expected baseline file path.
        baseline_path: PathBuf,
        /// Environment variable name that triggers update mode.
        update_env_var: String,
    },

    /// Semantic frame text did not match baseline.
    #[error("Frame snapshot '{name}' mismatch")]
    #[non_exhaustive]
    FrameMismatch {
        /// Name of the snapshot.
        name: String,
        /// Expected frame text.
        expected: String,
        /// Actual frame text.
        actual: String,
    },

    /// I/O error reading or writing files.
    #[error("I/O error: {0}")]
    IoError(String),

    /// Image format or loading error.
    #[error("Image error: {0}")]
    ImageError(String),
}

/// Compare two PNG screenshots and return the percentage of different pixels.
fn compare_screenshots(
    actual_path: &Path,
    reference_path: &Path,
    pixel_threshold: f64,
) -> Result<f64, SnapshotError> {
    let actual_img =
        image::open(actual_path).map_err(|err| SnapshotError::ImageError(err.to_string()))?;
    let reference_img =
        image::open(reference_path).map_err(|err| SnapshotError::ImageError(err.to_string()))?;

    let (actual_width, actual_height) = actual_img.dimensions();
    let (ref_width, ref_height) = reference_img.dimensions();

    if actual_width != ref_width || actual_height != ref_height {
        return Err(SnapshotError::Mismatch {
            name: "size".to_string(),
            diff_percent: 100.0,
            threshold: 0.0,
            baseline_path: reference_path.to_path_buf(),
            actual_path: actual_path.to_path_buf(),
        });
    }

    let total_pixels = f64::from(actual_width) * f64::from(actual_height);
    let mut different_pixels: f64 = 0.0;

    for pixel_y in 0..actual_height {
        for pixel_x in 0..actual_width {
            let actual_pixel = actual_img.get_pixel(pixel_x, pixel_y);
            let reference_pixel = reference_img.get_pixel(pixel_x, pixel_y);

            let distance = pixel_distance(actual_pixel.0, reference_pixel.0);
            if distance > pixel_threshold {
                different_pixels += 1.0;
            }
        }
    }

    Ok((different_pixels / total_pixels) * 100.0)
}

/// Compute the Euclidean distance between two RGBA pixel values (RGB only).
fn pixel_distance(pixel_a: [u8; 4], pixel_b: [u8; 4]) -> f64 {
    let red_diff = f64::from(pixel_a[0]) - f64::from(pixel_b[0]);
    let green_diff = f64::from(pixel_a[1]) - f64::from(pixel_b[1]);
    let blue_diff = f64::from(pixel_a[2]) - f64::from(pixel_b[2]);

    (red_diff * red_diff + green_diff * green_diff + blue_diff * blue_diff).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));

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
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
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
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"));
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
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"))
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
        let config =
            SnapshotConfig::new(temp.path().join("baselines"), temp.path().join("artifacts"))
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
}
