//! Generic feature demo builder: scenario execution with GIF generation.
//!
//! [`FeatureDemo`] bundles PTY scenario execution, proof collection, and
//! hash-cached VHS GIF generation into one reusable entry point. The caller
//! decides what to do with the [`FeatureResult`] artifacts — testty itself
//! has no opinion on static-site generators, README formats, or artifact
//! directories beyond GIF output.
//!
//! # Freshness mode
//!
//! [`FeatureDemo::gif_mode`] selects between three behaviors:
//!
//! - [`GifMode::GenerateIfStale`] (default) — preserves the historical
//!   behavior: skip VHS when the on-disk hash sidecar matches, otherwise
//!   regenerate.
//! - [`GifMode::CheckOnly`] — runs the scenario, computes the would-be hash,
//!   and reports whether the on-disk GIF is [`GifStatus::Fresh`] or
//!   [`GifStatus::Stale`] without invoking VHS. This path never mutates the
//!   filesystem, so it is safe on read-only CI mounts and when the GIF output
//!   directory does not exist yet. Useful for an agent or CI tool that wants to
//!   detect drift without paying VHS cost.
//! - [`GifMode::AlwaysGenerate`] — bypasses the hash cache and always re-runs
//!   VHS.

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

/// Selects how [`FeatureDemo::run`] handles GIF artifacts.
///
/// The variants correspond directly to the freshness behaviors documented
/// on the module docs: cache-respecting regeneration (default), hash-only
/// drift detection, and forced regeneration. Defaults to
/// [`GifMode::GenerateIfStale`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GifMode {
    /// Skip VHS when the on-disk hash sidecar matches; regenerate
    /// otherwise. Historical default behavior.
    #[default]
    GenerateIfStale,
    /// Compute the would-be hash and compare it to the on-disk sidecar
    /// without invoking VHS. Returns [`GifStatus::Fresh`] or
    /// [`GifStatus::Stale`].
    CheckOnly,
    /// Bypass the hash cache and regenerate the GIF unconditionally.
    ///
    /// VHS must be installed: this mode treats a missing VHS binary as a
    /// hard failure ([`GifStatus::TapeExecutionFailed`]) rather than the
    /// benign [`GifStatus::VhsNotInstalled`] skip used by other modes,
    /// because regeneration was explicitly requested.
    AlwaysGenerate,
}

/// Outcome of GIF generation during a [`FeatureDemo`] run.
///
/// Distinguishes intentional skips (VHS missing, cache hit, no output dir)
/// from unexpected failures (directory creation, tape execution) so callers
/// can log or fail appropriately.
///
/// `#[non_exhaustive]` so future variants stay non-breaking. Match arms must
/// include a fallback `_` arm.
#[derive(Debug)]
#[non_exhaustive]
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
    /// [`GifMode::CheckOnly`]: the on-disk GIF matches the current capture
    /// hash. No VHS execution was attempted.
    Fresh {
        /// Expected GIF path (may or may not exist on disk).
        gif_path: PathBuf,
        /// Hash computed from the current scenario captures.
        hash: u64,
    },
    /// [`GifMode::CheckOnly`]: the on-disk GIF is missing or its hash
    /// sidecar does not match the current capture hash. No VHS execution
    /// was attempted.
    Stale {
        /// Expected GIF path (may or may not exist on disk).
        gif_path: PathBuf,
        /// Hash computed from the current scenario captures.
        current: u64,
        /// Hash recorded in the on-disk sidecar, if any.
        committed: Option<u64>,
    },
}

impl GifStatus {
    /// Return the GIF path if generation succeeded, the cache matched, or a
    /// freshness check identified an expected output location.
    pub fn gif_path(&self) -> Option<&Path> {
        match self {
            Self::Generated(path) | Self::CacheHit(path) => Some(path),
            Self::Fresh { gif_path, .. } | Self::Stale { gif_path, .. } => Some(gif_path),
            _ => None,
        }
    }

    /// Return `true` when GIF generation failed unexpectedly.
    ///
    /// Intentional skips (`VhsNotInstalled`, `CacheHit`, `NoOutputDir`,
    /// `Fresh`, `Stale`) return `false`.
    pub fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::DirCreateFailed(_) | Self::TapeExecutionFailed(_)
        )
    }

    /// Return `true` when the on-disk GIF is known to be out of date with
    /// the current scenario captures. Only [`GifStatus::Stale`] returns
    /// `true`; every other variant returns `false`.
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
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
    /// Outcome of GIF generation (success, cache hit, skip, failure, or
    /// freshness verdict in [`GifMode::CheckOnly`]).
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
    gif_mode: GifMode,
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
            gif_mode: GifMode::default(),
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

    /// Select the GIF freshness mode. See [`GifMode`] for semantics.
    pub fn gif_mode(mut self, mode: GifMode) -> Self {
        self.gif_mode = mode;

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
                self.gif_mode,
                VhsContext {
                    settings: &self.gif_settings,
                    binary_path,
                    env_pairs,
                },
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

/// Compute a content hash from all proof capture frame bytes.
///
/// Uses [`DefaultHasher`] to produce a deterministic `u64` hash from the
/// concatenated frame bytes of every capture in the report. Exposed as
/// public API so external tooling (xtasks, CI freshness reports) can
/// reproduce the same hash that [`FeatureDemo::run`] writes to the on-disk
/// sidecar.
///
/// Note: `DefaultHasher` is not stable across Rust releases. Hashes
/// computed by one toolchain version should not be compared with hashes
/// produced by another. Within a single workspace toolchain pin this is
/// fine — and that is the contract testty assumes.
pub fn compute_frame_hash(report: &ProofReport) -> u64 {
    let mut hasher = DefaultHasher::new();

    for capture in &report.captures {
        normalized_frame_bytes_for_hash(&capture.frame_bytes).hash(&mut hasher);
    }

    hasher.finish()
}

/// Returns frame bytes with volatile temp roots normalized for hashing.
///
/// Feature tests often run inside fresh [`tempfile::TempDir`] directories,
/// while the captured TUI footer may display the absolute working directory.
/// Normalizing those paths keeps freshness sidecars tied to visible UI state
/// instead of one random temp directory name.
fn normalized_frame_bytes_for_hash(frame_bytes: &[u8]) -> Vec<u8> {
    let mut frame_text = String::from_utf8_lossy(frame_bytes).into_owned();

    for temp_root in temp_root_strings() {
        frame_text = frame_text.replace(&temp_root, "<tmp>");
    }

    normalize_tempfile_segments(&frame_text).into_bytes()
}

/// Returns temp root spellings that may appear in captured terminal frames.
fn temp_root_strings() -> Vec<String> {
    let temp_root = std::env::temp_dir();
    let mut roots = vec![
        temp_root
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string(),
    ];

    if let Ok(canonical_temp_root) = temp_root.canonicalize() {
        roots.push(
            canonical_temp_root
                .to_string_lossy()
                .trim_end_matches('/')
                .to_string(),
        );
    }

    roots.sort();
    roots.dedup();

    roots
}

/// Replaces random `tempfile` directory names after a normalized temp root.
fn normalize_tempfile_segments(frame_text: &str) -> String {
    const NORMALIZED_TEMPFILE_DIR: &str = "<tmp>/<tempdir>";
    const TEMPFILE_PREFIX: &str = "<tmp>/.tmp";

    let mut normalized = String::with_capacity(frame_text.len());
    let mut remainder = frame_text;

    while let Some(prefix_index) = remainder.find(TEMPFILE_PREFIX) {
        let after_prefix_index = prefix_index + TEMPFILE_PREFIX.len();
        normalized.push_str(&remainder[..prefix_index]);
        normalized.push_str(NORMALIZED_TEMPFILE_DIR);

        let after_prefix = &remainder[after_prefix_index..];
        let random_name_length = after_prefix
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        remainder = &after_prefix[random_name_length..];
    }

    normalized.push_str(remainder);

    normalized
}

/// Return the on-disk sidecar path that [`FeatureDemo`] uses to cache the
/// content hash for a feature with the given `name`.
///
/// Sidecars are stored as `.{name}.hash` next to the GIF (`{name}.gif`) so
/// the dot-prefix keeps them out of plain `ls` listings while staying in
/// the same directory as the artifact they describe.
pub fn hash_sidecar_path(output_dir: &Path, name: &str) -> PathBuf {
    output_dir.join(format!(".{name}.hash"))
}

/// Bundle of VHS-execution inputs threaded through [`generate_gif`].
///
/// Grouped so the function signature stays small while still exposing the
/// individual pieces (settings, binary, environment) that VHS needs.
#[derive(Clone, Copy)]
struct VhsContext<'a> {
    settings: &'a VhsTapeSettings,
    binary_path: &'a Path,
    env_pairs: &'a [(&'a str, &'a str)],
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
    mode: GifMode,
    vhs: VhsContext<'_>,
) -> GifStatus {
    let hash_path = hash_sidecar_path(output_dir, name);
    let gif_path = output_dir.join(format!("{name}.gif"));

    let current_hash = compute_frame_hash(report);
    let committed_hash = read_committed_hash(&hash_path);

    // CheckOnly is a read-only verification path: never mutate the
    // filesystem. It must work on read-only CI mounts and when the
    // output directory does not exist yet — a missing directory simply
    // means the GIF is missing, which is `Stale`.
    if matches!(mode, GifMode::CheckOnly) {
        let gif_present = gif_path.exists();
        let hash_matches = committed_hash == Some(current_hash);

        return if gif_present && hash_matches {
            GifStatus::Fresh {
                gif_path,
                hash: current_hash,
            }
        } else {
            GifStatus::Stale {
                gif_path,
                current: current_hash,
                committed: committed_hash,
            }
        };
    }

    // Probe VHS availability before mutating the filesystem so machines
    // without VHS skip cleanly even when the output directory is on a
    // read-only or permission-restricted mount.
    //
    // `AlwaysGenerate` is an explicit user request to regenerate, so a
    // missing VHS binary must surface as a hard failure rather than a
    // silent skip. Other modes treat a missing VHS as a benign skip and
    // return `VhsNotInstalled`.
    if let Err(err) = check_vhs_installed() {
        return vhs_missing_status(mode, err);
    }

    if let Err(err) = std::fs::create_dir_all(output_dir) {
        return GifStatus::DirCreateFailed(err);
    }

    if matches!(mode, GifMode::GenerateIfStale)
        && gif_path.exists()
        && committed_hash == Some(current_hash)
    {
        return GifStatus::CacheHit(gif_path);
    }

    let hash_string = current_hash.to_string();
    let screenshot_path = output_dir.join(format!("{name}.png"));

    let tape = VhsTape::from_scenario_with_settings(
        scenario,
        vhs.binary_path,
        &screenshot_path,
        vhs.env_pairs,
        vhs.settings,
    );

    let tape_path = output_dir.join(format!("{name}.tape"));

    match tape.execute(&tape_path) {
        Ok(_) => {
            let _ = std::fs::write(&hash_path, &hash_string);
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

/// Map a [`check_vhs_installed`] failure into a [`GifStatus`] based on the
/// active [`GifMode`].
///
/// `AlwaysGenerate` is an explicit user request to regenerate, so a
/// missing VHS binary surfaces as [`GifStatus::TapeExecutionFailed`].
/// Every other mode treats a missing VHS as the benign
/// [`GifStatus::VhsNotInstalled`] skip.
fn vhs_missing_status(mode: GifMode, err: VhsError) -> GifStatus {
    match mode {
        GifMode::AlwaysGenerate => GifStatus::TapeExecutionFailed(err),
        _ => GifStatus::VhsNotInstalled,
    }
}

/// Read the cached hash from an on-disk sidecar, returning `None` when the
/// file is missing or the contents do not parse as a `u64`.
fn read_committed_hash(hash_path: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(hash_path).ok()?;

    raw.trim().parse::<u64>().ok()
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
    fn feature_demo_default_mode_is_generate_if_stale() {
        // Arrange / Act
        let demo = FeatureDemo::new("mode_check");

        // Assert
        assert_eq!(demo.gif_mode, GifMode::GenerateIfStale);
    }

    #[test]
    fn feature_demo_gif_mode_configurable() {
        // Arrange / Act
        let demo = FeatureDemo::new("mode_check").gif_mode(GifMode::CheckOnly);

        // Assert
        assert_eq!(demo.gif_mode, GifMode::CheckOnly);
    }

    #[test]
    fn vhs_missing_status_always_generate_is_hard_failure() {
        // Arrange
        let err = VhsError::NotInstalled("missing".to_string());

        // Act
        let status = vhs_missing_status(GifMode::AlwaysGenerate, err);

        // Assert
        assert!(
            matches!(status, GifStatus::TapeExecutionFailed(_)),
            "AlwaysGenerate must surface missing VHS as a hard failure, got {status:?}",
        );
        assert!(status.is_failure());
    }

    #[test]
    fn vhs_missing_status_generate_if_stale_is_benign_skip() {
        // Arrange
        let err = VhsError::NotInstalled("missing".to_string());

        // Act
        let status = vhs_missing_status(GifMode::GenerateIfStale, err);

        // Assert
        assert!(matches!(status, GifStatus::VhsNotInstalled));
        assert!(!status.is_failure());
    }

    #[test]
    fn vhs_missing_status_check_only_is_benign_skip() {
        // Arrange — `CheckOnly` short-circuits before the VHS probe in
        // `generate_gif`, but the helper must still treat it as benign so
        // future refactors that route through it do not regress to a hard
        // failure.
        let err = VhsError::NotInstalled("missing".to_string());

        // Act
        let status = vhs_missing_status(GifMode::CheckOnly, err);

        // Assert
        assert!(matches!(status, GifStatus::VhsNotInstalled));
        assert!(!status.is_failure());
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
    fn normalized_frame_bytes_for_hash_removes_tempfile_directory_names() {
        // Arrange
        let temp_root = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|_| std::env::temp_dir())
            .to_string_lossy()
            .trim_end_matches('/')
            .to_string();
        let first_frame = format!("{temp_root}/.tmpAlpha123/test-project");
        let second_frame = format!("{temp_root}/.tmpBeta456/test-project");

        // Act
        let first_normalized = normalized_frame_bytes_for_hash(first_frame.as_bytes());
        let second_normalized = normalized_frame_bytes_for_hash(second_frame.as_bytes());

        // Assert
        assert_eq!(first_normalized, second_normalized);
        assert_eq!(
            String::from_utf8(first_normalized).expect("normalized frame should be utf8"),
            "<tmp>/<tempdir>/test-project",
        );
    }

    #[test]
    fn normalize_tempfile_segments_preserves_non_tempfile_paths() {
        // Arrange
        let frame_text = "<tmp>/stable-project";

        // Act
        let normalized = normalize_tempfile_segments(frame_text);

        // Assert
        assert_eq!(normalized, frame_text);
    }

    #[test]
    fn hash_sidecar_path_uses_dot_prefix_next_to_gif() {
        // Arrange
        let dir = Path::new("/tmp/features");

        // Act
        let sidecar = hash_sidecar_path(dir, "session_creation");

        // Assert
        assert_eq!(sidecar, Path::new("/tmp/features/.session_creation.hash"));
    }

    #[test]
    fn read_committed_hash_returns_none_for_missing_file() {
        // Arrange
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let missing = dir.path().join(".missing.hash");

        // Act
        let parsed = read_committed_hash(&missing);

        // Assert
        assert!(parsed.is_none());
    }

    #[test]
    fn read_committed_hash_parses_trimmed_decimal() {
        // Arrange
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let path = dir.path().join(".valid.hash");
        std::fs::write(&path, "  12345\n").expect("write hash");

        // Act
        let parsed = read_committed_hash(&path);

        // Assert
        assert_eq!(parsed, Some(12345));
    }

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
            settings: &settings,
            binary_path: binary,
            env_pairs,
        };

        // Act
        let status = generate_gif(
            &scenario,
            &report,
            "check_only_readonly",
            &missing_dir,
            GifMode::CheckOnly,
            vhs,
        );

        // Assert — verdict is Stale and the output directory is untouched.
        assert!(status.is_stale(), "expected stale verdict, got {status:?}");
        assert!(
            !missing_dir.exists(),
            "CheckOnly must not create the output directory",
        );
    }

    #[test]
    fn generate_gif_check_only_returns_fresh_when_gif_and_sidecar_match() {
        // Arrange — pre-stage a GIF file and a sidecar whose contents equal
        // the hash that `compute_frame_hash` would produce for the report.
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let name = "check_only_fresh";

        let frame = TerminalFrame::new(80, 24, b"Hello");
        let mut report = ProofReport::new(name);
        report.add_capture("snap", "Snapshot", &frame);

        let expected_hash = compute_frame_hash(&report);

        let gif_path = output_dir.join(format!("{name}.gif"));
        std::fs::write(&gif_path, b"fake-gif-bytes").expect("write fake gif");

        let sidecar = hash_sidecar_path(output_dir, name);
        std::fs::write(&sidecar, expected_hash.to_string()).expect("write sidecar");

        let scenario = Scenario::new(name);
        let settings = VhsTapeSettings::feature_demo();
        let binary = Path::new("/usr/bin/true");
        let env_pairs: &[(&str, &str)] = &[];
        let vhs = VhsContext {
            settings: &settings,
            binary_path: binary,
            env_pairs,
        };

        // Act
        let status = generate_gif(
            &scenario,
            &report,
            name,
            output_dir,
            GifMode::CheckOnly,
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
    fn read_committed_hash_returns_none_for_garbage() {
        // Arrange
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let path = dir.path().join(".garbage.hash");
        std::fs::write(&path, "not-a-number").expect("write hash");

        // Act
        let parsed = read_committed_hash(&path);

        // Assert
        assert!(parsed.is_none());
    }

    #[test]
    fn gif_status_generated_returns_path() {
        // Arrange
        let status = GifStatus::Generated(PathBuf::from("/tmp/test.gif"));

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/test.gif")));
        assert!(!status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_cache_hit_returns_path() {
        // Arrange
        let status = GifStatus::CacheHit(PathBuf::from("/tmp/cached.gif"));

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/cached.gif")));
        assert!(!status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_vhs_not_installed_is_not_failure() {
        // Arrange
        let status = GifStatus::VhsNotInstalled;

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(!status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_no_output_dir_is_not_failure() {
        // Arrange
        let status = GifStatus::NoOutputDir;

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(!status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_dir_create_failed_is_failure() {
        // Arrange
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let status = GifStatus::DirCreateFailed(err);

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_tape_execution_failed_is_failure() {
        // Arrange
        let err = VhsError::ExecutionFailed("vhs crashed".to_string());
        let status = GifStatus::TapeExecutionFailed(err);

        // Act / Assert
        assert!(status.gif_path().is_none());
        assert!(status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_fresh_exposes_path_and_is_not_stale() {
        // Arrange
        let status = GifStatus::Fresh {
            gif_path: PathBuf::from("/tmp/feature.gif"),
            hash: 42,
        };

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/feature.gif")));
        assert!(!status.is_failure());
        assert!(!status.is_stale());
    }

    #[test]
    fn gif_status_stale_exposes_path_and_is_stale() {
        // Arrange
        let status = GifStatus::Stale {
            gif_path: PathBuf::from("/tmp/feature.gif"),
            current: 42,
            committed: Some(7),
        };

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/feature.gif")));
        assert!(!status.is_failure());
        assert!(status.is_stale());
    }
}
