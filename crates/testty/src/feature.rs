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
//!   and reports whether the nonempty on-disk GIF is [`GifStatus::Fresh`] or
//!   [`GifStatus::Stale`] without invoking VHS. This path never mutates the
//!   filesystem, so it is safe on read-only CI mounts and when the GIF output
//!   directory does not exist yet. Useful for an agent or CI tool that wants to
//!   detect drift without paying VHS cost.
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
use crate::vhs::{VhsError, VhsTape, VhsTapeSettings, check_vhs_installed};

/// Metadata describing a feature demonstration.
///
/// Carries the human-readable name, title, and description that identify
/// the feature for downstream artifact generators (static-site pages,
/// README entries, etc.).
#[derive(Debug, Clone)]
pub struct FeatureMeta {
    /// Short description of the demonstrated behavior.
    pub description: String,
    /// Machine-readable identifier used in file names (e.g.
    /// `"session_creation"`).
    pub name: String,
    /// Human-readable title (e.g. `"Session creation"`).
    pub title: String,
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

/// Matching strategy backing one [`Redaction`].
#[derive(Debug, Clone)]
enum RedactionRule {
    /// Replace a bounded ASCII hex run that directly follows a prefix.
    HexAfter { prefix: String, max_hex_len: usize },
    /// Replace every occurrence of one exact string.
    Literal { needle: String },
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
    /// [`GifMode::CheckOnly`]: the on-disk GIF matches the current capture
    /// frame-and-render-settings hash. No VHS execution was attempted.
    Fresh {
        /// Expected GIF path (may or may not exist on disk).
        gif_path: PathBuf,
        /// Hash computed from the current scenario captures and VHS settings.
        hash: u64,
    },
    /// [`GifMode::CheckOnly`]: the on-disk GIF is missing or empty, or its
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
    /// Outcome of GIF generation (success, cache hit, skip, failure, or
    /// freshness verdict in [`GifMode::CheckOnly`]).
    pub gif_status: GifStatus,
    /// Feature metadata passed through from the builder.
    pub meta: FeatureMeta,
    /// Proof report with all labeled captures and diffs.
    pub report: ProofReport,
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
    gif_mode: GifMode,
    gif_output_dir: Option<PathBuf>,
    gif_settings: VhsTapeSettings,
    meta: FeatureMeta,
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
    /// When not set, GIF generation is skipped entirely. Regeneration records
    /// to a staging file and replaces the existing GIF only after VHS produces
    /// a nonempty file. A successful regeneration removes a same-named PNG so
    /// callers cannot mistake a poster from the previous GIF for a current
    /// artifact; a failed regeneration preserves the prior GIF, hash, and
    /// poster.
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
        let (frame, report) = scenario.run_with_proof(builder)?;

        let gif_status = match self.gif_output_dir.as_deref() {
            Some(output_dir) => generate_gif(
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
                    env_pairs,
                    execute_tape: VhsTape::execute,
                    settings: &self.gif_settings,
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
/// Uses a fixed FNV-1a `u64` hash over the concatenated frame bytes of every
/// capture in the report, after applying the built-in temp-root normalization
/// and the caller's `redactions`. This is the frame-only component of the GIF
/// freshness hash; use [`compute_gif_hash`] to reproduce the value written to
/// an on-disk sidecar.
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

/// Compute the deterministic hash written to a feature GIF sidecar.
///
/// Combines the normalized proof frames with every VHS rendering setting that
/// affects the generated artifact. External freshness tooling must pass the
/// same redactions and settings as [`FeatureDemo`] so frame or preset changes
/// invalidate the cached GIF.
pub fn compute_gif_hash(
    report: &ProofReport,
    redactions: &[Redaction],
    settings: &VhsTapeSettings,
) -> u64 {
    const SETTINGS_HASH_DOMAIN: &[u8] = b"\0testty-vhs-settings-v1\0";

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

    hash
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
/// Feature tests often run inside fresh `tempfile::TempDir` directories,
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
/// individual pieces (settings, binary, environment) that VHS needs.
#[derive(Clone, Copy)]
struct VhsContext<'a> {
    binary_path: &'a Path,
    check_vhs: fn() -> Result<(), VhsError>,
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

    let current_hash = compute_gif_hash(report, redactions, vhs.settings);
    let committed_hash = read_committed_hash(&hash_path);

    // CheckOnly is a read-only verification path: never mutate the
    // filesystem. It must work on read-only CI mounts and when the
    // output directory does not exist yet — a missing directory simply
    // means the GIF is missing, which is `Stale`.
    if matches!(mode, GifMode::CheckOnly) {
        let gif_present = is_nonempty_file(&gif_path);
        let hash_matches = committed_hash.value() == Some(current_hash);

        return if gif_present && hash_matches {
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
        && is_nonempty_file(&gif_path)
        && committed_hash.value() == Some(current_hash)
    {
        return GifStatus::CacheHit(gif_path);
    }

    // Trailing newline so the sidecar is a well-formed text file and
    // end-of-file fixers do not rewrite it after every regeneration.
    let hash_string = format!("{current_hash}\n");
    let poster_path = output_dir.join(format!("{name}.png"));
    let recording_path = output_dir.join(format!(".{name}.recording.gif"));
    let screenshot_path = output_dir.join(format!(".{name}.capture.png"));
    let tape_path = output_dir.join(format!("{name}.tape"));

    cleanup_recording_files(&tape_path, &screenshot_path, &recording_path);

    let tape = VhsTape::from_scenario_with_output_path(
        scenario,
        vhs.binary_path,
        &recording_path,
        &screenshot_path,
        vhs.env_pairs,
        vhs.settings,
    );

    let recording_result = (vhs.execute_tape)(&tape, &tape_path)
        .and_then(|_| finalize_gif_recording(&recording_path, &gif_path));
    cleanup_recording_files(&tape_path, &screenshot_path, &recording_path);

    match recording_result {
        Ok(()) => {
            let _ = std::fs::write(&hash_path, &hash_string);
            let _ = std::fs::remove_file(&poster_path);

            GifStatus::Generated(gif_path)
        }
        Err(err) => GifStatus::TapeExecutionFailed(err),
    }
}

/// Return whether `path` names a nonempty regular file.
fn is_nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// Replace the committed GIF only after VHS produces a valid staging file.
fn finalize_gif_recording(recording_path: &Path, gif_path: &Path) -> Result<(), VhsError> {
    if !is_nonempty_file(recording_path) {
        return Err(VhsError::ExecutionFailed(format!(
            "VHS did not produce a nonempty GIF at {}",
            recording_path.display(),
        )));
    }

    std::fs::rename(recording_path, gif_path).map_err(|err| {
        VhsError::IoError(format!(
            "failed to replace GIF {} with recording {}: {err}",
            gif_path.display(),
            recording_path.display(),
        ))
    })
}

/// Remove transient files produced while VHS records a staged GIF.
fn cleanup_recording_files(tape_path: &Path, screenshot_path: &Path, recording_path: &Path) {
    let _ = std::fs::remove_file(tape_path);
    let _ = std::fs::remove_file(screenshot_path);
    let _ = std::fs::remove_file(recording_path);
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
#[path = "feature_test.rs"]
mod tests;
