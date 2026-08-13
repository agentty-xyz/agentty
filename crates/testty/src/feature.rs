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
//!   and reports whether the nonempty on-disk GIF and PNG poster are
//!   [`GifStatus::Fresh`] or [`GifStatus::Stale`] without invoking VHS. This
//!   path never mutates the filesystem, so it is safe on read-only CI mounts
//!   and when the GIF output directory does not exist yet. Useful for an agent
//!   or CI tool that wants to detect drift without paying VHS cost.
//! - [`GifMode::AlwaysGenerate`] — bypasses the hash cache and always re-runs
//!   VHS.
//!
//! # Redaction
//!
//! The freshness hash only works when the same UI hashes the same way on every
//! run. Temp roots are normalized for free, but an application that paints its
//! own generated identifiers — session hashes, worktree names, short commit
//! ids — must declare them with [`FeatureDemo::redact`] so they stop counting
//! as UI drift.

use std::path::{Path, PathBuf};

use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::scenario::Scenario;
use crate::session::{PtySessionBuilder, PtySessionError};
use crate::vhs::{
    VHS_RECORDER_FINGERPRINT, VhsError, VhsTape, VhsTapeSettings, check_vhs_installed,
};

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

/// Caller-supplied rule that rewrites a generated hash before hashing a frame.
///
/// Applications under test often paint identifiers they generate at runtime —
/// a session hash, a worktree name, a short commit id. Those tokens change on
/// every run, so an unredacted frame hashes differently every time and the
/// committed GIF always looks stale. A [`Redaction`] replaces the volatile
/// token with a fixed placeholder, leaving the surrounding UI to drive the
/// hash.
///
/// Redaction affects only the freshness hash. Captured frames, assertions, and
/// the recorded GIF still show the real token.
///
/// # Example
///
/// ```
/// use testty::feature::Redaction;
///
/// // `wt/4175e5af` and `wt/9c0b17ff` hash identically.
/// let redaction = Redaction::hex_after("wt/", 8, "<hash>");
///
/// assert_eq!(redaction.apply("branch wt/4175e5af"), "branch wt/<hash>");
///
/// // A token the terminal cut off at the right edge is still redacted.
/// assert_eq!(redaction.apply("path .../wt/4175"), "path .../wt/<hash>");
///
/// // A known-volatile literal, such as the version the app paints in its
/// // header, hashes as its placeholder so releases do not stale every GIF.
/// let version = Redaction::literal("Agentty v0.13.0", "Agentty <version>");
///
/// assert_eq!(version.apply("Agentty v0.13.0 | FYI"), "Agentty <version> | FYI");
/// ```
#[derive(Debug, Clone)]
pub struct Redaction {
    placeholder: String,
    rule: RedactionRule,
}

/// Matching strategy backing one [`Redaction`].
#[derive(Debug, Clone)]
enum RedactionRule {
    /// Replace a bounded ASCII hex run that directly follows a prefix.
    HexAfter { prefix: String, max_hex_len: usize },
    /// Replace every occurrence of one exact string.
    Literal { needle: String },
}

impl Redaction {
    /// Redact a run of up to `max_hex_len` ASCII hex digits following `prefix`.
    ///
    /// The prefix anchors the rule: only hex runs that directly follow it are
    /// rewritten. A run longer than `max_hex_len` is left alone, so a rule for
    /// an 8-digit short hash never clips a full 40-digit one.
    ///
    /// Shorter runs are redacted because a TUI truncates: a hash painted at the
    /// right edge of the terminal shows however many digits happen to fit, and
    /// that count shifts with everything printed before it. Matching only the
    /// full-length token would leave those frames volatile.
    pub fn hex_after(
        prefix: impl Into<String>,
        max_hex_len: usize,
        placeholder: impl Into<String>,
    ) -> Self {
        Self {
            placeholder: placeholder.into(),
            rule: RedactionRule::HexAfter {
                prefix: prefix.into(),
                max_hex_len,
            },
        }
    }

    /// Redact every occurrence of the exact string `needle`.
    ///
    /// Use this for volatile text the caller can spell out ahead of time —
    /// typically a version string the application paints, which would
    /// otherwise stale every committed GIF hash on each release. The caller
    /// usually builds the needle from its own compile-time version so the
    /// rule tracks releases automatically.
    pub fn literal(needle: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Self {
            placeholder: placeholder.into(),
            rule: RedactionRule::Literal {
                needle: needle.into(),
            },
        }
    }

    /// Apply this rule to `text`, replacing every matching token.
    ///
    /// For [`Redaction::hex_after`] the prefix is preserved and only the hex
    /// token is replaced. An empty prefix or needle matches nothing and
    /// returns `text` unchanged.
    #[must_use]
    pub fn apply(&self, text: &str) -> String {
        match &self.rule {
            RedactionRule::HexAfter {
                prefix,
                max_hex_len,
            } => Self::apply_hex_after(text, prefix, *max_hex_len, &self.placeholder),
            RedactionRule::Literal { needle } => {
                if needle.is_empty() {
                    return text.to_string();
                }

                text.replace(needle, &self.placeholder)
            }
        }
    }

    /// Replaces bounded hex runs following `prefix` with `placeholder`.
    fn apply_hex_after(text: &str, prefix: &str, max_hex_len: usize, placeholder: &str) -> String {
        if prefix.is_empty() {
            return text.to_string();
        }

        let mut redacted = String::with_capacity(text.len());
        let mut remainder = text;

        while let Some(prefix_index) = remainder.find(prefix) {
            let after_prefix_index = prefix_index + prefix.len();
            let after_prefix = &remainder[after_prefix_index..];
            let token_len = after_prefix
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .count();

            redacted.push_str(&remainder[..after_prefix_index]);

            if (1..=max_hex_len).contains(&token_len) {
                redacted.push_str(placeholder);
                remainder = &after_prefix[token_len..];
            } else {
                remainder = after_prefix;
            }
        }

        redacted.push_str(remainder);

        redacted
    }
}

/// Selects how [`FeatureDemo::run`] handles GIF artifacts.
///
/// The variants correspond directly to the freshness behaviors documented
/// on the module docs: cache-respecting regeneration (default), hash-only
/// drift detection, and forced regeneration. Defaults to
/// [`GifMode::GenerateIfStale`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GifMode {
    /// Skip VHS when a nonempty GIF exists and the on-disk hash sidecar
    /// matches; regenerate otherwise. Historical default behavior.
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
    /// [`GifMode::CheckOnly`]: the on-disk GIF and PNG poster exist and the
    /// sidecar matches the current capture frame-and-render-settings hash. No
    /// VHS execution was attempted.
    Fresh {
        /// Expected GIF path (may or may not exist on disk).
        gif_path: PathBuf,
        /// Hash computed from the current scenario captures and VHS settings.
        hash: u64,
    },
    /// [`GifMode::CheckOnly`]: a published image is missing or empty, or the
    /// hash sidecar does not match the current frame-and-settings hash. No VHS
    /// execution was attempted.
    Stale {
        /// Expected GIF path (may or may not exist on disk).
        gif_path: PathBuf,
        /// Hash computed from the current scenario captures and VHS settings.
        current: u64,
        /// Hash recorded in the on-disk sidecar, if it exists and parses.
        committed: Option<u64>,
        /// Error found while reading or parsing the committed sidecar, if any.
        committed_error: Option<String>,
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
    redactions: Vec<Redaction>,
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
            redactions: Vec::new(),
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
    /// When not set, GIF generation is skipped entirely. Regeneration stages
    /// the GIF, hash, and final-capture PNG poster, then publishes the set with
    /// rollback protection. A failed regeneration preserves the prior GIF,
    /// hash, and poster.
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

    /// Declare a generated token the freshness hash must ignore.
    ///
    /// Rules apply in the order they are added, after the built-in temp-root
    /// normalization. See [`Redaction`] for what a rule matches.
    pub fn redact(mut self, redaction: Redaction) -> Self {
        self.redactions.push(redaction);

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
        self.run_with_assertion(scenario, builder, binary_path, env_pairs, |_, _| {})
    }

    /// Run the feature demo and assert its proof before GIF processing.
    ///
    /// The assertion receives the final frame and complete proof report from
    /// the same scenario execution whose captures determine GIF freshness.
    /// It runs before cache checks or recording, so a failed assertion cannot
    /// publish a demo that was not semantically verified.
    ///
    /// # Panics
    ///
    /// Panics from `assert` propagate to the caller before GIF processing.
    ///
    /// # Errors
    ///
    /// Returns a [`PtySessionError`] if scenario spawning or step execution
    /// fails.
    pub fn run_with_assertion(
        self,
        scenario: &Scenario,
        builder: PtySessionBuilder,
        binary_path: &Path,
        env_pairs: &[(&str, &str)],
        assert: impl FnOnce(&TerminalFrame, &ProofReport),
    ) -> Result<FeatureResult, PtySessionError> {
        self.run_with_assertion_and_recording_setup(
            scenario,
            builder,
            binary_path,
            env_pairs,
            assert,
            || Ok(()),
        )
    }

    /// Run an asserted feature demo after preparing its recording fixture.
    ///
    /// `prepare_recording` runs after the PTY proof and assertion but before
    /// any GIF freshness or recording work. A preparation failure becomes a
    /// [`GifStatus::TapeExecutionFailed`] result without changing published
    /// artifacts.
    ///
    /// # Panics
    ///
    /// Panics from `assert` propagate to the caller before GIF processing.
    ///
    /// # Errors
    ///
    /// Returns a [`PtySessionError`] if scenario spawning or step execution
    /// fails.
    pub fn run_with_assertion_and_recording_setup(
        self,
        scenario: &Scenario,
        builder: PtySessionBuilder,
        binary_path: &Path,
        env_pairs: &[(&str, &str)],
        assert: impl FnOnce(&TerminalFrame, &ProofReport),
        prepare_recording: impl FnOnce() -> Result<(), VhsError>,
    ) -> Result<FeatureResult, PtySessionError> {
        let (frame, report) = scenario.run_with_proof(builder)?;
        assert(&frame, &report);

        let gif_status = match self.gif_output_dir.as_deref() {
            Some(output_dir) => match prepare_recording() {
                Ok(()) => generate_gif(
                    scenario,
                    &report,
                    &self.meta.name,
                    output_dir,
                    GifContext {
                        mode: self.gif_mode,
                        redactions: &self.redactions,
                    },
                    VhsContext {
                        binary_path,
                        check_vhs: check_vhs_installed,
                        cleanup_after_recording: cleanup_recording_files,
                        env_pairs,
                        execute_tape: VhsTape::execute,
                        settings: &self.gif_settings,
                    },
                ),
                Err(error) => GifStatus::TapeExecutionFailed(error),
            },
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
/// Uses a fixed FNV-1a `u64` hash over the concatenated frame bytes of every
/// capture in the report, after applying the built-in temp-root normalization
/// and the caller's `redactions`. This is the frame-only component of the GIF
/// freshness hash; use [`compute_recording_hash`] to reproduce the value
/// written to an on-disk sidecar.
pub fn compute_frame_hash(report: &ProofReport, redactions: &[Redaction]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

    let mut hash = FNV_OFFSET_BASIS;
    for capture in &report.captures {
        update_fnv_hash(
            &mut hash,
            &normalized_frame_bytes_for_hash(&capture.frame_bytes, redactions),
        );
    }

    hash
}

/// Compute the deterministic frame-and-render-settings hash for a feature GIF.
///
/// Combines the normalized proof frames with every VHS rendering setting that
/// affects the generated artifact. [`FeatureDemo`] extends this value with the
/// canonical compiled recording specification and recorder fingerprint before
/// writing its sidecar.
pub fn compute_gif_hash(
    report: &ProofReport,
    redactions: &[Redaction],
    settings: &VhsTapeSettings,
) -> u64 {
    const SETTINGS_HASH_DOMAIN: &[u8] = b"\0testty-vhs-settings-v2\0";

    let mut hash = compute_frame_hash(report, redactions);
    update_fnv_hash(&mut hash, SETTINGS_HASH_DOMAIN);
    update_fnv_hash(&mut hash, &settings.width.to_le_bytes());
    update_fnv_hash(&mut hash, &settings.height.to_le_bytes());
    update_fnv_hash(&mut hash, &settings.font_size.to_le_bytes());
    update_fnv_hash(
        &mut hash,
        &u64::try_from(settings.theme.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    update_fnv_hash(&mut hash, settings.theme.as_bytes());
    update_fnv_hash(&mut hash, &settings.framerate.to_le_bytes());
    update_fnv_hash(&mut hash, &settings.padding.to_le_bytes());
    update_fnv_hash(
        &mut hash,
        &settings.letter_spacing().to_bits().to_le_bytes(),
    );
    update_fnv_hash(&mut hash, &settings.line_height().to_bits().to_le_bytes());

    hash
}

/// Compute the exact sidecar hash for one scenario and its compiled recording.
///
/// This is the supported freshness API for external tooling. It combines the
/// normalized proof frames, all VHS rendering settings, the canonical compiled
/// tape, and the recorder fingerprint. Callers must pass the same scenario,
/// redactions, and settings as [`FeatureDemo`] to reproduce its sidecar value.
pub fn compute_recording_hash(
    scenario: &Scenario,
    report: &ProofReport,
    redactions: &[Redaction],
    settings: &VhsTapeSettings,
) -> u64 {
    compute_recording_hash_with_identity(
        scenario,
        report,
        redactions,
        settings,
        VHS_RECORDER_FINGERPRINT,
    )
}

/// Compute a recording hash with an explicit recorder identity.
fn compute_recording_hash_with_identity(
    scenario: &Scenario,
    report: &ProofReport,
    redactions: &[Redaction],
    settings: &VhsTapeSettings,
    recorder_identity: &str,
) -> u64 {
    const RECORDING_HASH_DOMAIN: &[u8] = b"\0testty-vhs-recording-v3\0";
    const CANONICAL_BINARY_PATH: &str = "testty-canonical/bin";
    const CANONICAL_GIF_PATH: &str = "testty-canonical/output.gif";
    const CANONICAL_POSTER_PATH: &str = "testty-canonical/output.png";
    const CANONICAL_WORKDIR: &str = "testty-canonical/workdir";

    let canonical_env = [
        ("TESTTY_CANONICAL_ENV", "value"),
        ("PWD", CANONICAL_WORKDIR),
    ];
    let tape = VhsTape::from_scenario_with_output_path(
        scenario,
        Path::new(CANONICAL_BINARY_PATH),
        Path::new(CANONICAL_GIF_PATH),
        Path::new(CANONICAL_POSTER_PATH),
        &canonical_env,
        settings,
    );
    let mut hash = compute_gif_hash(report, redactions, settings);

    update_fnv_hash(&mut hash, RECORDING_HASH_DOMAIN);
    update_fnv_hash(&mut hash, recorder_identity.as_bytes());
    update_fnv_hash(&mut hash, tape.render().as_bytes());

    hash
}

/// Extend an FNV-1a hash with `bytes`.
fn update_fnv_hash(hash: &mut u64, bytes: &[u8]) {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

/// Returns frame bytes with volatile text normalized for hashing.
///
/// Feature tests often run inside fresh [`tempfile::TempDir`] directories,
/// while the captured TUI footer may display the absolute working directory.
/// Normalizing those paths keeps freshness sidecars tied to visible UI state
/// instead of one random temp directory name. Generated tokens the application
/// itself paints are the caller's to declare, through `redactions`.
fn normalized_frame_bytes_for_hash(frame_bytes: &[u8], redactions: &[Redaction]) -> Vec<u8> {
    let mut frame_text = String::from_utf8_lossy(frame_bytes).into_owned();

    for temp_root in temp_root_strings() {
        frame_text = frame_text.replace(&temp_root, "<tmp>");
    }

    frame_text = normalize_tempfile_segments(&frame_text);

    for redaction in redactions {
        frame_text = redaction.apply(&frame_text);
    }

    frame_text.into_bytes()
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

/// Bundle of freshness inputs threaded through [`generate_gif`].
///
/// Pairs the caller's [`GifMode`] with the redaction rules that decide what
/// counts as UI drift, so both travel together into the hash comparison.
#[derive(Clone, Copy)]
struct GifContext<'a> {
    mode: GifMode,
    redactions: &'a [Redaction],
}

/// Bundle of VHS-execution inputs threaded through [`generate_gif`].
///
/// Grouped so the function signature stays small while still exposing the
/// execution and cleanup boundaries plus the settings, binary, and environment
/// that VHS needs.
#[derive(Clone, Copy)]
struct VhsContext<'a> {
    binary_path: &'a Path,
    check_vhs: fn() -> Result<(), VhsError>,
    cleanup_after_recording: fn(&[&Path]) -> Result<(), VhsError>,
    env_pairs: &'a [(&'a str, &'a str)],
    execute_tape: fn(&VhsTape, &Path) -> Result<PathBuf, VhsError>,
    settings: &'a VhsTapeSettings,
}

/// Parsed state of a committed feature-GIF hash sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommittedHash {
    /// The sidecar file does not exist yet.
    Missing,
    /// The sidecar exists but cannot be read or parsed as a `u64`.
    Invalid(String),
    /// The sidecar contains a valid committed hash.
    Value(u64),
}

impl CommittedHash {
    /// Return the committed hash value when the sidecar parsed successfully.
    fn value(&self) -> Option<u64> {
        match self {
            Self::Value(hash) => Some(*hash),
            Self::Missing | Self::Invalid(_) => None,
        }
    }

    /// Return the sidecar read/parse error when the sidecar exists but is
    /// invalid.
    fn error(&self) -> Option<String> {
        match self {
            Self::Invalid(err) => Some(err.clone()),
            Self::Missing | Self::Value(_) => None,
        }
    }
}

/// Generate a GIF with content-hash caching, returning a typed status.
///
/// Checks VHS availability, computes a content hash from the proof frames and
/// VHS rendering settings, and skips VHS execution when the hash matches a
/// `.{name}.hash` sidecar file. Returns a [`GifStatus`] variant that
/// distinguishes intentional skips from unexpected failures.
fn generate_gif(
    scenario: &Scenario,
    report: &ProofReport,
    name: &str,
    output_dir: &Path,
    gif: GifContext<'_>,
    vhs: VhsContext<'_>,
) -> GifStatus {
    let GifContext { mode, redactions } = gif;

    let hash_path = hash_sidecar_path(output_dir, name);
    let gif_path = output_dir.join(format!("{name}.gif"));
    let poster_path = output_dir.join(format!("{name}.png"));

    let current_hash = compute_recording_hash(scenario, report, redactions, vhs.settings);
    let committed_hash = read_committed_hash(&hash_path);

    // CheckOnly is a read-only verification path: never mutate the
    // filesystem. It must work on read-only CI mounts and when the
    // output directory does not exist yet — a missing directory simply
    // means the GIF is missing, which is `Stale`.
    if matches!(mode, GifMode::CheckOnly) {
        let published_artifacts_present = published_artifacts_present(&gif_path, &poster_path);
        let hash_matches = committed_hash.value() == Some(current_hash);

        return if published_artifacts_present && hash_matches {
            GifStatus::Fresh {
                gif_path,
                hash: current_hash,
            }
        } else {
            GifStatus::Stale {
                gif_path,
                current: current_hash,
                committed: committed_hash.value(),
                committed_error: committed_hash.error(),
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
    if let Err(err) = (vhs.check_vhs)() {
        return vhs_missing_status(mode, err);
    }

    if let Err(err) = std::fs::create_dir_all(output_dir) {
        return GifStatus::DirCreateFailed(err);
    }

    if matches!(mode, GifMode::GenerateIfStale)
        && published_artifacts_present(&gif_path, &poster_path)
        && committed_hash.value() == Some(current_hash)
    {
        return GifStatus::CacheHit(gif_path);
    }

    // Trailing newline so the sidecar is a well-formed text file and
    // end-of-file fixers do not rewrite it after every regeneration.
    let hash_string = format!("{current_hash}\n");
    let recording_path = output_dir.join(format!(".{name}.recording.gif"));
    let recording_hash_path = output_dir.join(format!(".{name}.recording.hash"));
    let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
    let tape_path = output_dir.join(format!("{name}.tape"));

    if let Err(err) = cleanup_recording_files(&[
        &tape_path,
        &screenshot_path,
        &recording_path,
        &recording_hash_path,
    ]) {
        return GifStatus::TapeExecutionFailed(err);
    }

    let tape = VhsTape::from_scenario_with_output_path(
        scenario,
        vhs.binary_path,
        &recording_path,
        &screenshot_path,
        vhs.env_pairs,
        vhs.settings,
    );

    let recording_result = (vhs.execute_tape)(&tape, &tape_path).and_then(|_| {
        if !is_nonempty_file(&recording_path) {
            return Err(VhsError::ExecutionFailed(format!(
                "VHS did not produce a nonempty GIF at {}",
                recording_path.display(),
            )));
        }
        if !is_nonempty_file(&screenshot_path) {
            return Err(VhsError::ExecutionFailed(format!(
                "VHS did not produce a nonempty PNG poster at {}",
                screenshot_path.display(),
            )));
        }

        stage_hash_sidecar(&recording_hash_path, &hash_string)?;

        publish_gif_recording(
            &recording_path,
            &gif_path,
            &recording_hash_path,
            &hash_path,
            &screenshot_path,
            &poster_path,
        )
    });
    let cleanup_result = (vhs.cleanup_after_recording)(&[
        &tape_path,
        &screenshot_path,
        &recording_path,
        &recording_hash_path,
    ]);

    recording_publication_status(gif_path, recording_result, cleanup_result)
}

/// Preserve a committed publication even when transient-file cleanup fails.
fn recording_publication_status(
    gif_path: PathBuf,
    recording_result: Result<(), VhsError>,
    cleanup_result: Result<(), VhsError>,
) -> GifStatus {
    match recording_result {
        Ok(()) => {
            // Publication is committed. Cleanup cannot turn it into a failure
            // because downstream callers would roll back only the feature page.
            let _ = cleanup_result;

            GifStatus::Generated(gif_path)
        }
        Err(recording_err) => match cleanup_result {
            Ok(()) => GifStatus::TapeExecutionFailed(recording_err),
            Err(cleanup_err) => GifStatus::TapeExecutionFailed(VhsError::IoError(format!(
                "{recording_err}; cleanup also failed: {cleanup_err}"
            ))),
        },
    }
}

/// Return whether a published GIF and poster are both nonempty regular files.
fn published_artifacts_present(gif_path: &Path, poster_path: &Path) -> bool {
    is_nonempty_file(gif_path) && is_nonempty_file(poster_path)
}

/// Return whether `path` names a nonempty regular file.
fn is_nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// Write a staged hash sidecar with publication-specific error context.
fn stage_hash_sidecar(path: &Path, hash: &str) -> Result<(), VhsError> {
    std::fs::write(path, hash).map_err(|err| {
        VhsError::IoError(format!(
            "failed to stage hash sidecar {}: {err}",
            path.display(),
        ))
    })
}

/// Publish a staged GIF and hash as one rollback-safe artifact transaction.
fn publish_gif_recording(
    recording_path: &Path,
    gif_path: &Path,
    recording_hash_path: &Path,
    hash_path: &Path,
    recording_poster_path: &Path,
    poster_path: &Path,
) -> Result<(), VhsError> {
    publish_gif_recording_with_cleanup(
        recording_path,
        gif_path,
        recording_hash_path,
        hash_path,
        recording_poster_path,
        poster_path,
        cleanup_recording_files,
    )
}

/// Publish a staged artifact set with an injected post-commit cleanup step.
fn publish_gif_recording_with_cleanup(
    recording_path: &Path,
    gif_path: &Path,
    recording_hash_path: &Path,
    hash_path: &Path,
    recording_poster_path: &Path,
    poster_path: &Path,
    cleanup_backups: impl FnOnce(&[&Path]) -> Result<(), VhsError>,
) -> Result<(), VhsError> {
    let gif_backup_path = gif_path.with_extension("previous.gif");
    let hash_backup_path = hash_path.with_extension("previous.hash");
    let poster_backup_path = poster_path.with_extension("previous.png");
    validate_artifact_target(gif_path)?;
    validate_artifact_target(hash_path)?;
    validate_artifact_target(poster_path)?;
    let gif_existed = backup_artifact(gif_path, &gif_backup_path)?;
    let hash_existed = backup_artifact(hash_path, &hash_backup_path)?;
    let poster_existed = backup_artifact(poster_path, &poster_backup_path)?;

    let publish_result = replace_artifact(recording_path, gif_path, "GIF")
        .and_then(|()| replace_artifact(recording_hash_path, hash_path, "hash sidecar"))
        .and_then(|()| replace_artifact(recording_poster_path, poster_path, "PNG poster"));

    if let Err(publish_error) = publish_result {
        let rollback_errors = [
            restore_artifact(gif_path, &gif_backup_path, gif_existed),
            restore_artifact(hash_path, &hash_backup_path, hash_existed),
            restore_artifact(poster_path, &poster_backup_path, poster_existed),
        ]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();

        return Err(publication_error(publish_error, &rollback_errors));
    }

    // All replacements are committed at this point. Backup cleanup is
    // best-effort because reporting an error now would make downstream
    // publication roll back its page while retaining the new artifact set.
    let _ = cleanup_backups(&[&gif_backup_path, &hash_backup_path, &poster_backup_path]);

    Ok(())
}

/// Preserve a publication error and append any rollback failures.
fn publication_error(publish_error: VhsError, rollback_errors: &[String]) -> VhsError {
    if rollback_errors.is_empty() {
        publish_error
    } else {
        let rollback_errors = rollback_errors.join("; ");

        VhsError::IoError(format!(
            "{publish_error}; rollback failed: {rollback_errors}"
        ))
    }
}

/// Reject a directory or special file where publication expects an artifact.
fn validate_artifact_target(path: &Path) -> Result<(), VhsError> {
    if path.exists() && !path.is_file() {
        return Err(VhsError::IoError(format!(
            "artifact target is not a regular file: {}",
            path.display()
        )));
    }

    Ok(())
}

/// Copy an existing artifact to a transaction backup.
fn backup_artifact(path: &Path, backup_path: &Path) -> Result<bool, VhsError> {
    remove_file_if_exists(backup_path)?;

    if !path.exists() {
        return Ok(false);
    }
    std::fs::copy(path, backup_path).map_err(|err| {
        VhsError::IoError(format!(
            "failed to back up {} to {}: {err}",
            path.display(),
            backup_path.display(),
        ))
    })?;

    Ok(true)
}

/// Atomically replace one committed artifact with its staged file.
fn replace_artifact(staged_path: &Path, target_path: &Path, label: &str) -> Result<(), VhsError> {
    std::fs::rename(staged_path, target_path).map_err(|err| {
        VhsError::IoError(format!(
            "failed to replace {label} {} with staging file {}: {err}",
            target_path.display(),
            staged_path.display(),
        ))
    })
}

/// Restore one artifact from its transaction backup.
fn restore_artifact(path: &Path, backup_path: &Path, existed: bool) -> Result<(), VhsError> {
    if existed {
        std::fs::copy(backup_path, path).map_err(|err| {
            VhsError::IoError(format!(
                "failed to restore {} from {}: {err}",
                path.display(),
                backup_path.display(),
            ))
        })?;
    } else {
        remove_file_if_exists(path)?;
    }
    remove_file_if_exists(backup_path)
}

/// Remove transient files produced while VHS records a staged GIF.
fn cleanup_recording_files(paths: &[&Path]) -> Result<(), VhsError> {
    for path in paths {
        remove_file_if_exists(path)?;
    }

    Ok(())
}

/// Remove one file while treating an absent path as already clean.
fn remove_file_if_exists(path: &Path) -> Result<(), VhsError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(VhsError::IoError(format!(
            "failed to remove {}: {err}",
            path.display()
        ))),
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

/// Read the cached hash from an on-disk sidecar.
fn read_committed_hash(hash_path: &Path) -> CommittedHash {
    let raw = match std::fs::read_to_string(hash_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return CommittedHash::Missing,
        Err(err) => {
            return CommittedHash::Invalid(format!("failed to read hash sidecar: {err}"));
        }
    };

    match raw.trim().parse::<u64>() {
        Ok(hash) => CommittedHash::Value(hash),
        Err(err) => CommittedHash::Invalid(format!("failed to parse hash sidecar as u64: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATED_GIF_BYTES: &[u8] = b"generated gif";

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
    fn feature_demo_run_wires_check_only_vhs_context() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let scenario = Scenario::new("run_context")
            .wait_for_text("ready", 3_000)
            .capture_labeled("ready", "Shell is ready");
        let builder = PtySessionBuilder::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 60"]);
        let demo = FeatureDemo::new("run_context")
            .gif_output_dir(temp.path())
            .gif_mode(GifMode::CheckOnly);

        // Act
        let result = demo
            .run(&scenario, builder, Path::new("/bin/true"), &[])
            .expect("feature demo should run");

        // Assert
        assert!(matches!(result.gif_status, GifStatus::Stale { .. }));
        assert!(result.frame.all_text().contains("ready"));
    }

    #[test]
    fn feature_demo_run_with_assertion_validates_before_gif_processing() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let scenario = Scenario::new("validated_run")
            .wait_for_text("ready", 3_000)
            .capture_labeled("ready", "Shell is ready");
        let builder = PtySessionBuilder::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 60"]);
        let settings = VhsTapeSettings::feature_demo();
        let gif_path = output_dir.join("validated_run.gif");
        let poster_path = output_dir.join("validated_run.png");
        let hash_path = hash_sidecar_path(output_dir, "validated_run");
        let demo = FeatureDemo::new("validated_run")
            .gif_output_dir(output_dir)
            .gif_mode(GifMode::CheckOnly);

        // Act
        let result = demo
            .run_with_assertion(
                &scenario,
                builder,
                Path::new("/bin/true"),
                &[],
                |frame, report| {
                    assert!(frame.all_text().contains("ready"));
                    let hash = compute_recording_hash(&scenario, report, &[], &settings);
                    std::fs::write(&gif_path, b"validated gif").expect("write GIF");
                    std::fs::write(&poster_path, b"validated poster").expect("write poster");
                    std::fs::write(&hash_path, hash.to_string()).expect("write sidecar");
                },
            )
            .expect("validated feature demo should run");

        // Assert
        assert!(matches!(result.gif_status, GifStatus::Fresh { .. }));
    }

    #[test]
    fn feature_demo_recording_setup_failure_stops_gif_processing() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let scenario = Scenario::new("failed_recording_setup")
            .wait_for_text("ready", 3_000)
            .capture_labeled("ready", "Shell is ready");
        let builder = PtySessionBuilder::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 60"]);
        let gif_path = output_dir.join("failed_recording_setup.gif");

        // Act
        let result = FeatureDemo::new("failed_recording_setup")
            .gif_output_dir(output_dir)
            .run_with_assertion_and_recording_setup(
                &scenario,
                builder,
                Path::new("/bin/true"),
                &[],
                |frame, _report| assert!(frame.all_text().contains("ready")),
                || Err(VhsError::IoError("fixture restore failed".to_string())),
            )
            .expect("feature proof should run");

        // Assert
        assert!(matches!(
            result.gif_status,
            GifStatus::TapeExecutionFailed(VhsError::IoError(ref message))
                if message == "fixture restore failed"
        ));
        assert!(!gif_path.exists());
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
    fn feature_demo_collects_redactions_in_declaration_order() {
        // Arrange / Act
        let demo = FeatureDemo::new("redaction_check")
            .redact(Redaction::hex_after("wt/", 8, "<hash>"))
            .redact(Redaction::hex_after("commit ", 7, "<commit>"));

        // Assert
        let redacted: Vec<String> = demo
            .redactions
            .iter()
            .map(|redaction| redaction.apply("wt/4175e5af commit 9c0b17f"))
            .collect();

        assert_eq!(
            redacted,
            vec![
                "wt/<hash> commit 9c0b17f".to_string(),
                "wt/4175e5af commit <commit>".to_string(),
            ],
        );
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
        let hash_a = compute_frame_hash(&report, &[]);
        let hash_b = compute_frame_hash(&report, &[]);

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
        let hash_a = compute_frame_hash(&report_a, &[]);
        let hash_b = compute_frame_hash(&report_b, &[]);

        // Assert
        assert_ne!(hash_a, hash_b);
    }

    #[test]
    fn compute_frame_hash_empty_report() {
        // Arrange
        let report = ProofReport::new("empty");

        // Act
        let hash = compute_frame_hash(&report, &[]);

        // Assert — empty reports use the stable FNV-1a offset basis.
        assert_eq!(hash, 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn compute_frame_hash_ignores_redacted_tokens() {
        // Arrange — the same UI showing two different generated hashes.
        let frame_a = TerminalFrame::new(80, 24, b"branch wt/4175e5af");
        let frame_b = TerminalFrame::new(80, 24, b"branch wt/9c0b17ff");

        let mut report_a = ProofReport::new("a");
        report_a.add_capture("snap", "A", &frame_a);

        let mut report_b = ProofReport::new("b");
        report_b.add_capture("snap", "B", &frame_b);

        let redactions = [Redaction::hex_after("wt/", 8, "<hash>")];

        // Act
        let hash_a = compute_frame_hash(&report_a, &redactions);
        let hash_b = compute_frame_hash(&report_b, &redactions);

        // Assert — with the token redacted the two frames hash alike.
        assert_eq!(hash_a, hash_b);
        assert_ne!(
            compute_frame_hash(&report_a, &[]),
            compute_frame_hash(&report_b, &[]),
            "without the redaction the same UI must still hash differently",
        );
    }

    #[test]
    fn compute_gif_hash_includes_every_render_setting() {
        // Arrange
        let frame = TerminalFrame::new(80, 24, b"Hello");
        let mut report = ProofReport::new("settings_hash");
        report.add_capture("snap", "Snapshot", &frame);
        let settings = VhsTapeSettings::feature_demo();
        let expected_hash = compute_gif_hash(&report, &[], &settings);

        let mut different_width = settings.clone();
        different_width.width += 1;
        let mut different_height = settings.clone();
        different_height.height += 1;
        let mut different_font_size = settings.clone();
        different_font_size.font_size += 1;
        let mut different_theme = settings.clone();
        different_theme.theme.push_str("-variant");
        let mut different_framerate = settings.clone();
        different_framerate.framerate += 1;
        let mut different_padding = settings;
        different_padding.padding += 1;
        let variants = [
            different_width,
            different_height,
            different_font_size,
            different_theme,
            different_framerate,
            different_padding,
        ];

        // Act
        let variant_hashes = variants
            .iter()
            .map(|variant| compute_gif_hash(&report, &[], variant))
            .collect::<Vec<_>>();

        // Assert
        assert!(variant_hashes.iter().all(|hash| *hash != expected_hash));
    }

    #[test]
    fn recording_hash_includes_compiled_scenario() {
        // Arrange
        let report = ProofReport::new("scenario_hash");
        let settings = VhsTapeSettings::feature_demo();
        let short_pause = Scenario::new("scenario_hash").viewing_pause_ms(500);
        let long_pause = Scenario::new("scenario_hash").viewing_pause_ms(1500);

        // Act
        let short_hash = compute_recording_hash(&short_pause, &report, &[], &settings);
        let long_hash = compute_recording_hash(&long_pause, &report, &[], &settings);

        // Assert
        assert_ne!(short_hash, long_hash);
    }

    #[test]
    fn recording_hash_includes_recorder_identity() {
        // Arrange
        let scenario = Scenario::new("recorder_hash").capture();
        let report = ProofReport::new("recorder_hash");
        let settings = VhsTapeSettings::feature_demo();

        // Act
        let first_hash =
            compute_recording_hash_with_identity(&scenario, &report, &[], &settings, "vhs@first");
        let second_hash =
            compute_recording_hash_with_identity(&scenario, &report, &[], &settings, "vhs@second");

        // Assert
        assert_ne!(first_hash, second_hash);
    }

    #[test]
    fn redaction_replaces_every_matching_token() {
        // Arrange — the worktree path and the branch label both carry the hash.
        let redaction = Redaction::hex_after("wt/", 8, "<hash>");
        let frame_text = "<tmp>/<tempdir>/wt/4175e5af  wt/4175e5af";

        // Act
        let redacted = redaction.apply(frame_text);

        // Assert
        assert_eq!(redacted, "<tmp>/<tempdir>/wt/<hash>  wt/<hash>");
    }

    #[test]
    fn redaction_replaces_a_token_cut_off_by_the_terminal_edge() {
        // Arrange — the footer path runs past the right edge, so the frame
        // keeps only the leading digits of the hash.
        let redaction = Redaction::hex_after("wt/", 8, "<hash>");

        // Act
        let short = redaction.apply("<tmp>/<tempdir>/agentty_root/wt/53a");
        let shorter = redaction.apply("<tmp>/<tempdir>/agentty_root/wt/5");

        // Assert — however many digits survive, the frame reads the same.
        assert_eq!(short, "<tmp>/<tempdir>/agentty_root/wt/<hash>");
        assert_eq!(shorter, short);
    }

    #[test]
    fn redaction_preserves_runs_longer_than_the_rule() {
        // Arrange — an 8-digit rule must not clip a full-length hash, and a
        // non-hex label after the prefix is not a hash at all.
        let redaction = Redaction::hex_after("wt/", 8, "<hash>");
        let frame_text = "wt/4175e5afff  wt/topic";

        // Act
        let redacted = redaction.apply(frame_text);

        // Assert
        assert_eq!(redacted, frame_text);
    }

    #[test]
    fn redaction_with_empty_prefix_is_inert() {
        // Arrange
        let redaction = Redaction::hex_after("", 8, "<hash>");
        let frame_text = "wt/4175e5af";

        // Act
        let redacted = redaction.apply(frame_text);

        // Assert
        assert_eq!(redacted, frame_text);
    }

    #[test]
    fn redaction_literal_replaces_every_occurrence() {
        // Arrange — the header paints the version once per captured frame.
        let redaction = Redaction::literal("Agentty v0.13.0", "Agentty <version>");
        let frame_text = "Agentty v0.13.0 | FYI\nAgentty v0.13.0";

        // Act
        let redacted = redaction.apply(frame_text);

        // Assert
        assert_eq!(redacted, "Agentty <version> | FYI\nAgentty <version>");
    }

    #[test]
    fn redaction_literal_with_empty_needle_is_inert() {
        // Arrange
        let redaction = Redaction::literal("", "<version>");
        let frame_text = "Agentty v0.13.0";

        // Act
        let redacted = redaction.apply(frame_text);

        // Assert
        assert_eq!(redacted, frame_text);
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
        let first_normalized = normalized_frame_bytes_for_hash(first_frame.as_bytes(), &[]);
        let second_normalized = normalized_frame_bytes_for_hash(second_frame.as_bytes(), &[]);

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
    fn read_committed_hash_returns_missing_for_missing_file() {
        // Arrange
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let missing = dir.path().join(".missing.hash");

        // Act
        let parsed = read_committed_hash(&missing);

        // Assert
        assert_eq!(parsed, CommittedHash::Missing);
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
        assert_eq!(parsed, CommittedHash::Value(12345));
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
            binary_path: binary,
            check_vhs: check_vhs_installed,
            cleanup_after_recording: cleanup_recording_files,
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
        let scenario = Scenario::new(name);
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);

        let gif_path = output_dir.join(format!("{name}.gif"));
        let poster_path = output_dir.join(format!("{name}.png"));
        std::fs::write(&gif_path, b"fake-gif-bytes").expect("write fake gif");
        std::fs::write(&poster_path, b"fake-poster-bytes").expect("write fake poster");

        let sidecar = hash_sidecar_path(output_dir, name);
        std::fs::write(&sidecar, expected_hash.to_string()).expect("write sidecar");

        let binary = Path::new("/usr/bin/true");
        let env_pairs: &[(&str, &str)] = &[];
        let vhs = VhsContext {
            binary_path: binary,
            check_vhs: check_vhs_installed,
            cleanup_after_recording: cleanup_recording_files,
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
        let scenario = Scenario::new(name);
        let committed_hash = compute_recording_hash(&scenario, &report, &[], &previous_settings);
        let current_settings = VhsTapeSettings::feature_demo();
        let current_hash = compute_recording_hash(&scenario, &report, &[], &current_settings);

        let gif_path = output_dir.join(format!("{name}.gif"));
        std::fs::write(&gif_path, b"previous-preset-gif").expect("write GIF");
        let sidecar = hash_sidecar_path(output_dir, name);
        std::fs::write(&sidecar, committed_hash.to_string()).expect("write sidecar");

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
        let scenario = Scenario::new(name);
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);
        let gif_path = output_dir.join(format!("{name}.gif"));
        let hash_path = hash_sidecar_path(output_dir, name);
        std::fs::write(&gif_path, []).expect("write empty gif");
        std::fs::write(&hash_path, expected_hash.to_string()).expect("write sidecar");
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
            cleanup_after_recording: cleanup_recording_files,
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
        let scenario = Scenario::new(name);
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);
        let gif_path = output_dir.join(format!("{name}.gif"));
        let poster_path = output_dir.join(format!("{name}.png"));
        let hash_path = hash_sidecar_path(output_dir, name);
        std::fs::write(&gif_path, GENERATED_GIF_BYTES).expect("write cached gif");
        std::fs::write(&poster_path, b"cached poster").expect("write cached poster");
        std::fs::write(&hash_path, expected_hash.to_string()).expect("write sidecar");
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
        let scenario = Scenario::new(name).capture();
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);
        let gif_path = output_dir.join(format!("{name}.gif"));
        let hash_path = hash_sidecar_path(output_dir, name);
        std::fs::write(&gif_path, []).expect("write empty gif");
        std::fs::write(&hash_path, expected_hash.to_string()).expect("write sidecar");
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
    fn generate_gif_success_publishes_poster_and_cleans_recording_files() {
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
        let recording_hash_path = output_dir.join(format!(".{name}.recording.hash"));
        let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
        let tape_path = output_dir.join(format!("{name}.tape"));
        let settings = VhsTapeSettings::feature_demo();
        let scenario = Scenario::new(name).capture();
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);
        std::fs::write(&gif_path, b"previous gif").expect("write previous gif");
        std::fs::write(&poster_path, b"stale poster").expect("write poster");
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
        assert_eq!(
            std::fs::read(&poster_path).expect("read poster"),
            b"temporary screenshot"
        );
        assert!(!recording_path.exists());
        assert!(!recording_hash_path.exists());
        assert!(!screenshot_path.exists());
        assert!(!tape_path.exists());
    }

    #[test]
    fn generate_gif_success_ignores_post_publication_tape_cleanup_failure() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let name = "generated_with_cleanup_failure";
        let frame = TerminalFrame::new(80, 24, b"Hello");
        let mut report = ProofReport::new(name);
        report.add_capture("snap", "Snapshot", &frame);
        let gif_path = output_dir.join(format!("{name}.gif"));
        let hash_path = hash_sidecar_path(output_dir, name);
        let poster_path = output_dir.join(format!("{name}.png"));
        let tape_path = output_dir.join(format!("{name}.tape"));
        let settings = VhsTapeSettings::feature_demo();
        let scenario = Scenario::new(name).capture();
        let expected_hash = compute_recording_hash(&scenario, &report, &[], &settings);
        let mut vhs = test_vhs_context(&settings, vhs_available, successful_tape_execution);
        vhs.cleanup_after_recording = fail_recording_cleanup;

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
        assert_eq!(
            std::fs::read(&poster_path).expect("read poster"),
            b"temporary screenshot"
        );
        assert!(tape_path.exists());
    }

    #[test]
    fn generate_gif_failure_combines_recording_and_tape_cleanup_errors() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let name = "failed_with_cleanup_failure";
        let report = ProofReport::new(name);
        let scenario = Scenario::new(name).capture();
        let settings = VhsTapeSettings::feature_demo();
        let mut vhs = test_vhs_context(&settings, vhs_available, failed_tape_execution);
        vhs.cleanup_after_recording = fail_recording_cleanup;

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
                if err.to_string().contains("simulated failure")
                    && err.to_string().contains("simulated tape cleanup failure")
        ));
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
    fn generate_gif_rejects_an_unremovable_transient_path() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let name = "blocked_transient_cleanup";
        let tape_path = output_dir.join(format!("{name}.tape"));
        std::fs::create_dir(&tape_path).expect("create conflicting tape directory");
        let report = ProofReport::new(name);
        let scenario = Scenario::new(name).capture();
        let settings = VhsTapeSettings::feature_demo();
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
        assert!(matches!(
            status,
            GifStatus::TapeExecutionFailed(VhsError::IoError(ref message))
                if message.contains("failed to remove") && message.contains(".tape")
        ));
    }

    #[test]
    fn generate_gif_rejects_a_missing_recording_poster() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let output_dir = temp.path();
        let name = "missing_recording_poster";
        let report = ProofReport::new(name);
        let scenario = Scenario::new(name).capture();
        let settings = VhsTapeSettings::feature_demo();
        let vhs = test_vhs_context(&settings, vhs_available, missing_poster_tape_execution);

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
            GifStatus::TapeExecutionFailed(VhsError::ExecutionFailed(ref message))
                if message.contains("did not produce a nonempty PNG poster")
        ));
    }

    #[test]
    fn stage_hash_sidecar_reports_write_failure() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let hash_path = temp.path().join("recording.hash");
        std::fs::create_dir(&hash_path).expect("create conflicting hash directory");

        // Act
        let error = stage_hash_sidecar(&hash_path, "42\n")
            .expect_err("hash staging should reject a directory");

        // Assert
        assert!(error.to_string().contains("failed to stage hash sidecar"));
        assert!(error.to_string().contains("recording.hash"));
    }

    #[test]
    fn publish_gif_recording_rejects_invalid_target_before_publication() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let recording_path = temp.path().join("recording.gif");
        let gif_path = temp.path().join("feature.gif");
        let recording_hash_path = temp.path().join("recording.hash");
        let recording_poster_path = temp.path().join("recording.png");
        let hash_path = temp.path().join("feature.hash");
        let poster_path = temp.path().join("feature.png");
        std::fs::write(&gif_path, b"previous gif").expect("write previous gif");
        std::fs::write(&poster_path, b"previous poster").expect("write previous poster");
        std::fs::write(&recording_path, GENERATED_GIF_BYTES).expect("write recording");
        std::fs::write(&recording_hash_path, b"new hash\n").expect("write recording hash");
        std::fs::write(&recording_poster_path, b"new poster").expect("write recording poster");
        std::fs::create_dir(&hash_path).expect("create conflicting hash directory");

        // Act
        let result = publish_gif_recording(
            &recording_path,
            &gif_path,
            &recording_hash_path,
            &hash_path,
            &recording_poster_path,
            &poster_path,
        );

        // Assert
        assert!(matches!(result, Err(VhsError::IoError(_))));
        assert_eq!(std::fs::read(&gif_path).expect("read gif"), b"previous gif");
        assert_eq!(
            std::fs::read(&poster_path).expect("read poster"),
            b"previous poster"
        );
        assert!(recording_path.exists());
        assert!(recording_hash_path.exists());
        assert!(hash_path.is_dir());
        assert!(!gif_path.with_extension("previous.gif").exists());
    }

    #[test]
    fn publish_gif_recording_rolls_back_partial_publication() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let recording_path = temp.path().join("recording.gif");
        let gif_path = temp.path().join("feature.gif");
        let missing_recording_hash_path = temp.path().join("missing-recording.hash");
        let recording_poster_path = temp.path().join("recording.png");
        let hash_path = temp.path().join("feature.hash");
        let poster_path = temp.path().join("feature.png");
        std::fs::write(&recording_path, GENERATED_GIF_BYTES).expect("write recording");
        std::fs::write(&gif_path, b"previous gif").expect("write previous gif");
        std::fs::write(&hash_path, b"previous hash\n").expect("write previous hash");
        std::fs::write(&poster_path, b"previous poster").expect("write previous poster");
        std::fs::write(&recording_poster_path, b"new poster").expect("write recording poster");

        // Act
        let result = publish_gif_recording(
            &recording_path,
            &gif_path,
            &missing_recording_hash_path,
            &hash_path,
            &recording_poster_path,
            &poster_path,
        );

        // Assert
        assert!(matches!(result, Err(VhsError::IoError(_))));
        assert_eq!(std::fs::read(&gif_path).expect("read gif"), b"previous gif");
        assert_eq!(
            std::fs::read(&hash_path).expect("read hash"),
            b"previous hash\n"
        );
        assert_eq!(
            std::fs::read(&poster_path).expect("read poster"),
            b"previous poster"
        );
        assert!(!gif_path.with_extension("previous.gif").exists());
        assert!(!hash_path.with_extension("previous.hash").exists());
        assert!(!poster_path.with_extension("previous.png").exists());
    }

    #[test]
    fn publish_gif_recording_keeps_committed_set_when_backup_cleanup_fails() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let recording_path = temp.path().join("recording.gif");
        let gif_path = temp.path().join("feature.gif");
        let recording_hash_path = temp.path().join("recording.hash");
        let hash_path = temp.path().join("feature.hash");
        let recording_poster_path = temp.path().join("recording.png");
        let poster_path = temp.path().join("feature.png");
        std::fs::write(&recording_path, b"new gif").expect("write recording");
        std::fs::write(&gif_path, b"previous gif").expect("write previous gif");
        std::fs::write(&recording_hash_path, b"new hash\n").expect("write recording hash");
        std::fs::write(&hash_path, b"previous hash\n").expect("write previous hash");
        std::fs::write(&recording_poster_path, b"new poster").expect("write recording poster");
        std::fs::write(&poster_path, b"previous poster").expect("write previous poster");

        // Act
        let result = publish_gif_recording_with_cleanup(
            &recording_path,
            &gif_path,
            &recording_hash_path,
            &hash_path,
            &recording_poster_path,
            &poster_path,
            |_| {
                Err(VhsError::IoError(
                    "simulated backup cleanup failure".to_string(),
                ))
            },
        );

        // Assert
        assert!(result.is_ok());
        assert_eq!(std::fs::read(&gif_path).expect("read GIF"), b"new gif");
        assert_eq!(std::fs::read(&hash_path).expect("read hash"), b"new hash\n");
        assert_eq!(
            std::fs::read(&poster_path).expect("read poster"),
            b"new poster"
        );
        assert_eq!(
            std::fs::read(gif_path.with_extension("previous.gif")).expect("read GIF backup"),
            b"previous gif"
        );
        assert_eq!(
            std::fs::read(hash_path.with_extension("previous.hash")).expect("read hash backup"),
            b"previous hash\n"
        );
        assert_eq!(
            std::fs::read(poster_path.with_extension("previous.png")).expect("read poster backup"),
            b"previous poster"
        );
    }

    #[test]
    fn publication_error_combines_publish_and_rollback_failures() {
        // Arrange
        let publish_error = VhsError::IoError("publish failed".to_string());
        let rollback_errors = vec![
            "GIF restore failed".to_string(),
            "poster restore failed".to_string(),
        ];

        // Act
        let error = publication_error(publish_error, &rollback_errors);

        // Assert
        let message = error.to_string();
        assert!(message.contains("publish failed"));
        assert!(message.contains("GIF restore failed; poster restore failed"));
    }

    #[test]
    fn backup_artifact_reports_copy_failure() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let artifact_path = temp.path().join("feature.gif");
        let backup_path = temp.path().join("feature.previous.gif");
        std::fs::create_dir(&artifact_path).expect("create invalid artifact directory");

        // Act
        let result = backup_artifact(&artifact_path, &backup_path);

        // Assert
        let error = result.expect_err("copying an artifact directory should fail");
        assert!(error.to_string().contains("failed to back up"));
    }

    #[test]
    fn restore_artifact_reports_missing_backup() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let artifact_path = temp.path().join("feature.gif");
        let backup_path = temp.path().join("missing.previous.gif");

        // Act
        let error = restore_artifact(&artifact_path, &backup_path, true)
            .expect_err("missing backup should fail restoration");

        // Assert
        assert!(error.to_string().contains("failed to restore"));
        assert!(error.to_string().contains("missing.previous.gif"));
    }

    #[test]
    fn restore_artifact_removes_new_target() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let artifact_path = temp.path().join("feature.gif");
        let backup_path = temp.path().join("feature.previous.gif");
        std::fs::write(&artifact_path, b"new gif").expect("write new artifact");

        // Act
        restore_artifact(&artifact_path, &backup_path, false)
            .expect("remove newly published artifact");

        // Assert
        assert!(!artifact_path.exists());
        assert!(!backup_path.exists());
    }

    #[test]
    fn remove_file_if_exists_reports_non_file_target() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let directory = temp.path().join("artifact.gif");
        std::fs::create_dir(&directory).expect("create artifact directory");

        // Act
        let error = remove_file_if_exists(&directory)
            .expect_err("directory removal through file boundary should fail");

        // Assert
        assert!(error.to_string().contains("failed to remove"));
        assert!(error.to_string().contains("artifact.gif"));
    }

    #[test]
    fn read_committed_hash_reports_io_error() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temp dir");
        let hash_path = temp.path().join("feature.hash");
        std::fs::create_dir(&hash_path).expect("create hash directory");

        // Act
        let committed_hash = read_committed_hash(&hash_path);

        // Assert
        assert!(matches!(
            committed_hash,
            CommittedHash::Invalid(ref message)
                if message.contains("failed to read hash sidecar")
        ));
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

    #[test]
    fn read_committed_hash_returns_invalid_for_garbage() {
        // Arrange
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        let path = dir.path().join(".garbage.hash");
        std::fs::write(&path, "not-a-number").expect("write hash");

        // Act
        let parsed = read_committed_hash(&path);

        // Assert
        let CommittedHash::Invalid(err) = parsed else {
            unreachable!("expected invalid hash sidecar, got {parsed:?}");
        };

        assert!(err.contains("failed to parse hash sidecar"));
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
            committed_error: None,
        };

        // Act / Assert
        assert_eq!(status.gif_path(), Some(Path::new("/tmp/feature.gif")));
        assert!(!status.is_failure());
        assert!(status.is_stale());
    }

    fn test_vhs_context(
        settings: &VhsTapeSettings,
        check_vhs: fn() -> Result<(), VhsError>,
        execute_tape: fn(&VhsTape, &Path) -> Result<PathBuf, VhsError>,
    ) -> VhsContext<'_> {
        VhsContext {
            binary_path: Path::new("/usr/bin/true"),
            check_vhs,
            cleanup_after_recording: cleanup_recording_files,
            env_pairs: &[],
            execute_tape,
            settings,
        }
    }

    fn vhs_available() -> Result<(), VhsError> {
        std::fs::metadata(".")
            .map(|_| ())
            .map_err(|err| VhsError::IoError(err.to_string()))
    }

    fn vhs_unavailable() -> Result<(), VhsError> {
        Err(VhsError::NotInstalled(
            "VHS unavailable in test".to_string(),
        ))
    }

    fn successful_tape_execution(tape: &VhsTape, tape_path: &Path) -> Result<PathBuf, VhsError> {
        stage_tape_execution(tape, tape_path, GENERATED_GIF_BYTES)
    }

    fn failed_tape_execution(tape: &VhsTape, tape_path: &Path) -> Result<PathBuf, VhsError> {
        let _ = stage_tape_execution(tape, tape_path, GENERATED_GIF_BYTES)?;

        Err(VhsError::ExecutionFailed("simulated failure".to_string()))
    }

    fn empty_tape_execution(tape: &VhsTape, tape_path: &Path) -> Result<PathBuf, VhsError> {
        stage_tape_execution(tape, tape_path, &[])
    }

    fn missing_poster_tape_execution(
        tape: &VhsTape,
        tape_path: &Path,
    ) -> Result<PathBuf, VhsError> {
        tape.write_to(tape_path)
            .map_err(|err| VhsError::IoError(err.to_string()))?;
        std::fs::write(tape_gif_path(tape), GENERATED_GIF_BYTES)
            .map_err(|err| VhsError::IoError(err.to_string()))?;

        Ok(tape.screenshot_path().to_path_buf())
    }

    fn fail_recording_cleanup(_paths: &[&Path]) -> Result<(), VhsError> {
        Err(VhsError::IoError(
            "simulated tape cleanup failure".to_string(),
        ))
    }

    fn stage_tape_execution(
        tape: &VhsTape,
        tape_path: &Path,
        gif_bytes: &[u8],
    ) -> Result<PathBuf, VhsError> {
        tape.write_to(tape_path)
            .map_err(|err| VhsError::IoError(err.to_string()))?;
        std::fs::write(tape_gif_path(tape), gif_bytes)
            .map_err(|err| VhsError::IoError(err.to_string()))?;
        std::fs::write(tape.screenshot_path(), b"temporary screenshot")
            .map_err(|err| VhsError::IoError(err.to_string()))?;

        Ok(tape.screenshot_path().to_path_buf())
    }

    fn tape_gif_path(tape: &VhsTape) -> PathBuf {
        const OUTPUT_PREFIX: &str = "Output \"";

        let output_line = tape
            .render()
            .lines()
            .find(|line| line.starts_with(OUTPUT_PREFIX))
            .expect("tape must declare GIF output");
        let path = output_line
            .strip_prefix(OUTPUT_PREFIX)
            .and_then(|line| line.strip_suffix('"'))
            .expect("GIF output must be double quoted");

        PathBuf::from(path)
    }
}
