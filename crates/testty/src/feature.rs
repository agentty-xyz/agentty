//! Generic feature demo builder: scenario execution with GIF generation.
//!
//! [`FeatureDemo`] bundles PTY scenario execution, proof collection, and
//! hash-cached VHS GIF generation into one reusable entry point. The caller
//! decides what to do with the [`FeatureResult`] artifacts — testty itself
//! has no opinion on static-site generators, README formats, or artifact
//! directories beyond GIF output.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::scenario::Scenario;
use crate::session::{PtySessionBuilder, PtySessionError};
use crate::vhs::{VhsError, VhsTape, VhsTapeSettings, check_vhs_installed};

/// Metadata describing a feature demonstration.
///
/// Carries the human-readable name, title, and description that identify
/// the feature for downstream artifact generators (static-site pages,
/// README entries, etc.).
#[derive(Debug, Clone)]
pub struct FeatureMeta {
    /// Machine-readable identifier used in file names (e.g.
    /// `"session_creation"`).
    pub name: String,
    /// Human-readable title (e.g. `"Session creation"`).
    pub title: String,
    /// Short description of the demonstrated behavior.
    pub description: String,
}

/// Outcome of GIF generation during a [`FeatureDemo`] run.
///
/// Distinguishes intentional skips (VHS missing, cache hit, no output dir)
/// from unexpected failures (directory creation, tape execution) so callers
/// can log or fail appropriately.
#[derive(Debug)]
pub enum GifStatus {
    /// GIF was generated successfully at the given path.
    Generated(PathBuf),
    /// GIF already existed and the content hash matched — skipped
    /// regeneration.
    CacheHit(PathBuf),
    /// VHS is not installed; GIF generation was skipped.
    VhsNotInstalled,
    /// No output directory was configured; GIF generation was skipped.
    NoOutputDir,
    /// GIF output directory could not be created.
    DirCreateFailed(std::io::Error),
    /// VHS tape execution failed.
    TapeExecutionFailed(VhsError),
}

impl GifStatus {
    /// Return the GIF path if generation succeeded or the cache matched.
    pub fn gif_path(&self) -> Option<&Path> {
        match self {
            Self::Generated(path) | Self::CacheHit(path) => Some(path),
            _ => None,
        }
    }

    /// Return `true` when GIF generation failed unexpectedly.
    ///
    /// Intentional skips (`VhsNotInstalled`, `CacheHit`, `NoOutputDir`)
    /// return `false`.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::DirCreateFailed(_) | Self::TapeExecutionFailed(_)
        )
    }
}

/// Artifacts produced by a [`FeatureDemo`] run.
///
/// Contains the final terminal frame, the full proof report with labeled
/// captures, the feature metadata, and the GIF generation status.
pub struct FeatureResult {
    /// Final terminal frame after scenario execution.
    pub frame: TerminalFrame,
    /// Proof report with all labeled captures and diffs.
    pub report: ProofReport,
    /// Feature metadata passed through from the builder.
    pub meta: FeatureMeta,
    /// Outcome of GIF generation (success, cache hit, skip, or failure).
    pub gif_status: GifStatus,
}

/// Generic feature demo builder: scenario + GIF with hash caching.
///
/// Owns scenario execution lifecycle and optional VHS GIF generation with
/// content-hash caching. The caller provides the [`PtySessionBuilder`],
/// binary path, and environment pairs for VHS tape compilation.
///
/// # Example
///
/// ```ignore
/// let scenario = Scenario::new("tab_switch")
///     .compose(&startup_journey)
///     .press_key("Tab")
///     .capture_labeled("after", "After tab press");
///
/// let result = FeatureDemo::new("tab_switch")
///     .title("Tab switching")
///     .description("Press Tab to cycle through tabs.")
///     .gif_output_dir("docs/static/features")
///     .run(&scenario, builder, &binary_path, &env_pairs)
///     .expect("feature demo failed");
/// ```
#[must_use]
pub struct FeatureDemo {
    meta: FeatureMeta,
    gif_output_dir: Option<PathBuf>,
    gif_settings: VhsTapeSettings,
}

impl FeatureDemo {
    /// Create a new feature demo builder with the given name.
    ///
    /// Title and description default to the name until overridden.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();

        Self {
            meta: FeatureMeta {
                title: name.clone(),
                description: String::new(),
                name,
            },
            gif_output_dir: None,
            gif_settings: VhsTapeSettings::feature_demo(),
        }
    }

    /// Set the human-readable title for this feature.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.meta.title = title.into();

        self
    }

    /// Set the short description for this feature.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.meta.description = description.into();

        self
    }

    /// Set the directory where GIF output and hash sidecars are written.
    ///
    /// When not set, GIF generation is skipped entirely.
    pub fn gif_output_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.gif_output_dir = Some(dir.into());

        self
    }

    /// Override the default [`VhsTapeSettings::feature_demo()`] settings.
    pub fn gif_settings(mut self, settings: VhsTapeSettings) -> Self {
        self.gif_settings = settings;

        self
    }

    /// Run the feature demo: execute the scenario, collect proof, and
    /// optionally generate a hash-cached GIF.
    ///
    /// The caller provides the scenario to execute, the PTY session builder,
    /// and the binary path + environment pairs for VHS tape compilation.
    ///
    /// # Errors
    ///
    /// Returns a [`PtySessionError`] if scenario spawning or step
    /// execution fails.
    pub fn run(
        self,
        scenario: &Scenario,
        builder: PtySessionBuilder,
        binary_path: &Path,
        env_pairs: &[(&str, &str)],
    ) -> Result<FeatureResult, PtySessionError> {
        let (frame, report) = scenario.run_with_proof(builder)?;

        let gif_status = match self.gif_output_dir.as_deref() {
            Some(output_dir) => generate_gif(
                scenario,
                &report,
                &self.meta.name,
                output_dir,
                &self.gif_settings,
                binary_path,
                env_pairs,
            ),
            None => GifStatus::NoOutputDir,
        };

        Ok(FeatureResult {
            frame,
            report,
            meta: self.meta,
            gif_status,
        })
    }
}

/// Generate a GIF with content-hash caching, returning a typed status.
///
/// Checks VHS availability, computes a content hash from all proof capture
/// frame bytes, and skips VHS execution when the hash matches a
/// `.{name}.hash` sidecar file. Returns a [`GifStatus`] variant that
/// distinguishes intentional skips from unexpected failures.
fn generate_gif(
    scenario: &Scenario,
    report: &ProofReport,
    name: &str,
    output_dir: &Path,
    settings: &VhsTapeSettings,
    binary_path: &Path,
    env_pairs: &[(&str, &str)],
) -> GifStatus {
    if check_vhs_installed().is_err() {
        return GifStatus::VhsNotInstalled;
    }

    if let Err(err) = std::fs::create_dir_all(output_dir) {
        return GifStatus::DirCreateFailed(err);
    }

    let hash_path = output_dir.join(format!(".{name}.hash"));
    let gif_path = output_dir.join(format!("{name}.gif"));

    let current_hash = compute_frame_hash(report);
    let hash_string = current_hash.to_string();

    // Check sidecar cache — skip VHS when the GIF already matches.
    if gif_path.exists()
        && let Ok(cached) = std::fs::read_to_string(&hash_path)
        && cached.trim() == hash_string
    {
        return GifStatus::CacheHit(gif_path);
    }

    // Build and execute VHS tape.
    let screenshot_path = output_dir.join(format!("{name}.png"));

    let tape = VhsTape::from_scenario_with_settings(
        scenario,
        binary_path,
        &screenshot_path,
        env_pairs,
        settings,
    );

    let tape_path = output_dir.join(format!("{name}.tape"));

    match tape.execute(&tape_path) {
        Ok(_) => {
            // Best-effort hash sidecar write — skip on failure.
            let _ = std::fs::write(&hash_path, &hash_string);

            // Clean up temporary files.
            let _ = std::fs::remove_file(&tape_path);
            let _ = std::fs::remove_file(&screenshot_path);

            GifStatus::Generated(gif_path)
        }
        Err(err) => {
            let _ = std::fs::remove_file(&tape_path);

            GifStatus::TapeExecutionFailed(err)
        }
    }
}

/// Compute a content hash from all proof capture frame bytes.
///
/// Uses [`DefaultHasher`] to produce a deterministic `u64` hash from the
/// concatenated frame bytes of every capture in the report.
fn compute_frame_hash(report: &ProofReport) -> u64 {
    let mut hasher = DefaultHasher::new();

    for capture in &report.captures {
        capture.frame_bytes.hash(&mut hasher);
    }

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_demo_builder_sets_metadata() {
        // Arrange / Act
        let demo = FeatureDemo::new("test_feature")
            .title("Test Feature")
            .description("A test description.");

        // Assert
        assert_eq!(demo.meta.name, "test_feature");
        assert_eq!(demo.meta.title, "Test Feature");
        assert_eq!(demo.meta.description, "A test description.");
    }

    #[test]
    fn feature_demo_defaults_title_to_name() {
        // Arrange / Act
        let demo = FeatureDemo::new("my_feature");

        // Assert
        assert_eq!(demo.meta.title, "my_feature");
        assert_eq!(demo.meta.description, "");
    }

    #[test]
    fn feature_demo_defaults_to_feature_demo_gif_settings() {
        // Arrange / Act
        let demo = FeatureDemo::new("settings_check");
        let expected = VhsTapeSettings::feature_demo();

        // Assert
        assert_eq!(demo.gif_settings.width, expected.width);
        assert_eq!(demo.gif_settings.height, expected.height);
        assert_eq!(demo.gif_settings.font_size, expected.font_size);
        assert_eq!(demo.gif_settings.theme, expected.theme);
    }

    #[test]
    fn feature_demo_gif_output_dir_configurable() {
        // Arrange / Act
        let demo = FeatureDemo::new("dir_check").gif_output_dir("/tmp/gifs");

        // Assert
        assert_eq!(demo.gif_output_dir.as_deref(), Some(Path::new("/tmp/gifs")));
    }

    #[test]
    fn feature_demo_no_gif_dir_means_none() {
        // Arrange / Act
        let demo = FeatureDemo::new("no_gif");

        // Assert
        assert!(demo.gif_output_dir.is_none());
    }

    #[test]
    fn compute_frame_hash_deterministic() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello");
        let mut report = ProofReport::new("hash_test");
        report.add_capture("snap", "Snapshot", &frame);

        // Act
        let hash_a = compute_frame_hash(&report);
        let hash_b = compute_frame_hash(&report);

        // Assert
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn compute_frame_hash_differs_for_different_content() {
        // Arrange
        let frame_a = TerminalFrame::new(80, 24, b"Hello");
        let frame_b = TerminalFrame::new(80, 24, b"World");

        let mut report_a = ProofReport::new("a");
        report_a.add_capture("snap", "A", &frame_a);

        let mut report_b = ProofReport::new("b");
        report_b.add_capture("snap", "B", &frame_b);

        // Act
        let hash_a = compute_frame_hash(&report_a);
        let hash_b = compute_frame_hash(&report_b);

        // Assert
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn compute_frame_hash_empty_report() {
        // Arrange
        let report = ProofReport::new("empty");

        // Act
        let hash = compute_frame_hash(&report);

        // Assert — should produce a consistent hash even with no captures.
        assert_eq!(hash, compute_frame_hash(&report));
    }

    #[test]
    fn gif_status_generated_returns_path() {
        // Arrange
        let status = GifStatus::Generated(PathBuf::from("/tmp/test.gif"));

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/test.gif")));
        assert!(!status.is_failure());
    }

    #[test]
    fn gif_status_cache_hit_returns_path() {
        // Arrange
        let status = GifStatus::CacheHit(PathBuf::from("/tmp/cached.gif"));

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/cached.gif")));
        assert!(!status.is_failure());
    }

    #[test]
    fn gif_status_vhs_not_installed_is_not_failure() {
        // Arrange
        let status = GifStatus::VhsNotInstalled;

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(!status.is_failure());
    }

    #[test]
    fn gif_status_no_output_dir_is_not_failure() {
        // Arrange
        let status = GifStatus::NoOutputDir;

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(!status.is_failure());
    }

    #[test]
    fn gif_status_dir_create_failed_is_failure() {
        // Arrange
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let status = GifStatus::DirCreateFailed(err);

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(status.is_failure());
    }

    #[test]
    fn gif_status_tape_execution_failed_is_failure() {
        // Arrange
        let err = VhsError::ExecutionFailed("vhs crashed".to_string());
        let status = GifStatus::TapeExecutionFailed(err);

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(status.is_failure());
    }
}
