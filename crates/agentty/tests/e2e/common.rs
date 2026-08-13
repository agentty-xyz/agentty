//! Shared helpers and agentty-specific `Journey` builders for E2E tests.
//!
//! Provides [`BuilderEnv`] for isolated test environments and
//! [`FeatureTest`] for declarative feature demo tests with optional Zola
//! page generation.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use agentty::db::{DB_DIR, DB_FILE, Database, DbError};
use assert_cmd::cargo::cargo_bin;
use testty::assertion;
use testty::feature::{FeatureDemo, GifMode, GifStatus, Redaction};
use testty::frame::TerminalFrame;
use testty::journey::Journey;
use testty::proof::report::{ProofCapture, ProofReport};
use testty::region::Region;
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;
use testty::step::Step;
use testty::vhs::VhsError;

/// CLI executable names stubbed into every [`BuilderEnv`] `PATH`.
///
/// Mirrors `AgentKind::ALL` executable names in `ag-agent`. All supported
/// CLIs are stubbed — not just `claude` — so agent availability does not
/// depend on which real CLIs a machine happens to have installed. A partial
/// stub set would let the machine decide the default agent a new session
/// resolves, painting different frames locally and on CI.
const STUB_AGENT_EXECUTABLES: [&str; 4] = ["agy", "claude", "codex", "gemini"];

/// Isolated test environment carrying `agentty_root` and `workdir` paths.
///
/// Use [`BuilderEnv::new`] to create a fresh environment under a temporary
/// directory, [`BuilderEnv::builder`] to get a configured
/// [`PtySessionBuilder`], and [`BuilderEnv::as_vhs_env_pairs`] to export the
/// environment for VHS tape compilation.
pub(crate) struct BuilderEnv {
    /// Path used as `AGENTTY_ROOT` for database and session isolation.
    pub(crate) agentty_root: PathBuf,
    /// Directory used as `HOME` so project discovery stays isolated from
    /// developer and CI machine repositories.
    pub(crate) home_dir: PathBuf,
    /// Directory containing stub agent executables so the app passes startup
    /// availability validation even when no real agent CLI is installed.
    pub(crate) stub_bin: PathBuf,
    /// Deterministic working directory registered as a project on startup.
    pub(crate) workdir: PathBuf,
}

impl BuilderEnv {
    /// Create a new isolated environment under `temp_root`.
    ///
    /// Creates `agentty_root` and `test-project` subdirectories so each test
    /// gets a fresh database and deterministic project name.
    ///
    /// Every directory the UI can paint lives under `home_dir`, so the app
    /// renders it home-collapsed (`~/test-project`, `~/.agentty/wt/<hash>`).
    /// An absolute temp path would be truncated differently per platform —
    /// macOS temp roots are far longer than Linux's `/tmp` — which made the
    /// same UI paint different frames and defeat GIF freshness hashing. The
    /// temp root is canonicalized first because the app registers its physical
    /// working directory (`getcwd`), and the home-prefix collapse only fires
    /// when `HOME` is spelled the same way.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub(crate) fn new(temp_root: &Path) -> std::io::Result<Self> {
        let temp_root = temp_root.canonicalize()?;
        let home_dir = temp_root.join("home");
        let agentty_root = home_dir.join(".agentty");
        let workdir = home_dir.join("test-project");
        let stub_bin = temp_root.join("stub-bin");

        std::fs::create_dir_all(&agentty_root)?;
        std::fs::create_dir_all(&home_dir)?;
        std::fs::create_dir_all(&workdir)?;
        std::fs::create_dir_all(&stub_bin)?;

        // Create a stub executable for every supported agent CLI. The stubs
        // are prepended to `PATH`, so they shadow any real CLI installed on
        // the machine. That keeps agent availability — and everything derived
        // from it, like the default agent a new session resolves — identical
        // on developer machines and CI, so the same scenario paints the same
        // frames everywhere and GIF freshness hashes stay reproducible.
        for executable_name in STUB_AGENT_EXECUTABLES
            .into_iter()
            .filter(|executable_name| *executable_name != "gemini")
        {
            let stub_agent_path = stub_bin.join(executable_name);
            let stub_version_path = stub_bin.join(format!("{executable_name}.version"));
            let initial_version = if executable_name == "agy" {
                "1.2.0"
            } else {
                "0.0.0-test"
            };
            let updated_version = if executable_name == "agy" {
                "1.2.1"
            } else {
                "0.0.1-updated"
            };
            let stub_script = format!(
                "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then printf '{updated_version}\\n' > \
                 \"{}\"; exit 0; fi\nif [ \"$1\" = \"--version\" ]; then if [ -f \"{}\" ]; then \
                 read version < \"{}\"; else version='{initial_version}'; fi; printf '{} %s\\n' \
                 \"$version\"; exit 0; fi\nexit 1\n",
                stub_version_path.display(),
                stub_version_path.display(),
                stub_version_path.display(),
                executable_name,
            );
            std::fs::write(&stub_agent_path, stub_script)?;
            std::fs::set_permissions(&stub_agent_path, std::fs::Permissions::from_mode(0o755))?;
        }
        Self::create_gemini_cli_stub(&stub_bin)?;

        Ok(Self {
            agentty_root,
            home_dir,
            stub_bin,
            workdir,
        })
    }

    /// Creates an npm-global Gemini CLI fixture whose package-manager update
    /// changes the version reported by the linked `gemini` executable.
    fn create_gemini_cli_stub(stub_bin: &Path) -> std::io::Result<()> {
        let gemini_package_directory = stub_bin.join("lib/node_modules/@google/gemini-cli/bundle");
        let gemini_package_path = gemini_package_directory.join("gemini.js");
        let gemini_path = stub_bin.join("gemini");
        let npm_path = stub_bin.join("npm");
        let version_path = stub_bin.join("gemini.version");
        std::fs::create_dir_all(&gemini_package_directory)?;
        std::fs::write(
            &gemini_package_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 91; fi\nif [ \"$1\" = \
                 \"--version\" ]; then if [ -f \"{}\" ]; then read version < \"{}\"; else \
                 version='0.0.0-test'; fi; printf 'gemini %s\\n' \"$version\"; exit 0; fi\nexit \
                 1\n",
                version_path.display(),
                version_path.display(),
            ),
        )?;
        std::fs::write(
            &npm_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"install\" ] && [ \"$2\" = \"-g\" ] && [ \"$3\" = \
                 \"@google/gemini-cli@latest\" ]; then printf '0.0.1-updated\\n' > \"{}\"; exit \
                 0; fi\nexit 1\n",
                version_path.display(),
            ),
        )?;
        std::fs::set_permissions(&gemini_package_path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&npm_path, std::fs::Permissions::from_mode(0o755))?;
        symlink(&gemini_package_path, &gemini_path)?;

        Ok(())
    }

    /// Return a configured [`PtySessionBuilder`] using this environment.
    ///
    /// Sets `AGENTTY_ROOT`, working directory, 80×24 terminal size, and
    /// prepends the stub agent bin directory to `PATH` so the app passes
    /// startup agent availability validation.
    pub(crate) fn builder(&self) -> PtySessionBuilder {
        let path_with_stub_bin = self.path_with_stub_bin();

        self.builder_with_path_and_size(
            path_with_stub_bin,
            DEFAULT_TERMINAL_COLS,
            DEFAULT_TERMINAL_ROWS,
        )
    }

    /// Return a configured [`PtySessionBuilder`] with an explicit `PATH` and
    /// terminal size.
    ///
    /// This keeps feature tests able to choose between inheriting system
    /// commands, using only deterministic stub agent executables, and
    /// exercising responsive layouts at their intended widths.
    fn builder_with_path_and_size(
        &self,
        path_env: String,
        terminal_cols: u16,
        terminal_rows: u16,
    ) -> PtySessionBuilder {
        PtySessionBuilder::new(cargo_bin("agentty"))
            .size(terminal_cols, terminal_rows)
            .env("AGENTTY_ROOT", self.agentty_root.to_string_lossy())
            .env("HOME", self.home_dir.to_string_lossy())
            .env(NO_COLOR_ENV_VAR, NO_COLOR_ENV_VALUE)
            .env("PATH", path_env)
            .workdir(&self.workdir)
    }

    /// Return environment variable pairs for VHS tape compilation.
    ///
    /// These match the variables set by [`BuilderEnv::builder`] so the VHS
    /// recording reproduces the same environment as the PTY session.
    pub(crate) fn as_vhs_env_pairs(&self) -> Vec<(String, String)> {
        self.as_vhs_env_pairs_with_path(self.path_with_stub_bin())
    }

    /// Return VHS environment variable pairs with an explicit `PATH`.
    ///
    /// This mirrors [`BuilderEnv::builder_with_path_and_size`] so PTY proof
    /// runs and VHS recordings see the same command lookup environment.
    fn as_vhs_env_pairs_with_path(&self, path_env: String) -> Vec<(String, String)> {
        vec![
            (
                "AGENTTY_ROOT".to_string(),
                self.agentty_root.to_string_lossy().into_owned(),
            ),
            (
                "HOME".to_string(),
                self.home_dir.to_string_lossy().into_owned(),
            ),
            (NO_COLOR_ENV_VAR.to_string(), NO_COLOR_ENV_VALUE.to_string()),
            ("PATH".to_string(), path_env),
            (
                "PWD".to_string(),
                self.workdir.to_string_lossy().into_owned(),
            ),
        ]
    }

    /// Build a `PATH` value with the stub bin directory prepended to the
    /// inherited system `PATH`.
    fn path_with_stub_bin(&self) -> String {
        let system_path = std::env::var("PATH").unwrap_or_default();
        let mut paths = vec![self.stub_bin.clone()];
        paths.extend(std::env::split_paths(&system_path));

        match std::env::join_paths(paths) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(_) => self.stub_bin.to_string_lossy().into_owned(),
        }
    }

    /// Build a deterministic `PATH` that exposes only the test stub bin.
    fn stub_only_path(&self) -> String {
        self.stub_bin.to_string_lossy().into_owned()
    }
}

/// Session row shape inserted by [`seed_session`].
#[derive(Clone, Copy)]
enum SessionSeedKind<'a> {
    /// Insert a normal persisted session row.
    Regular,
    /// Insert a draft session row that has not started running.
    Draft,
    /// Insert a stacked draft session linked to an existing parent.
    StackedDraft {
        /// Parent session id used by the stack relationship.
        parent_session_id: &'a str,
        /// Worktree path stored on the stacked draft session.
        worktree_path: &'a str,
    },
}

/// Declarative seed data for inserting one E2E session row.
#[derive(Clone, Copy)]
pub(crate) struct SessionSeed<'a> {
    base_branch: &'a str,
    kind: SessionSeedKind<'a>,
    model: &'a str,
    project_git_branch: Option<&'a str>,
    session_id: &'a str,
    status: &'a str,
    title: Option<&'a str>,
}

impl<'a> SessionSeed<'a> {
    /// Build seed data for a regular persisted session.
    pub(crate) fn regular(
        session_id: &'a str,
        model: &'a str,
        base_branch: &'a str,
        status: &'a str,
    ) -> Self {
        Self {
            base_branch,
            kind: SessionSeedKind::Regular,
            model,
            project_git_branch: Some(base_branch),
            session_id,
            status,
            title: None,
        }
    }

    /// Build seed data for an unstarted draft session.
    pub(crate) fn draft(
        session_id: &'a str,
        model: &'a str,
        base_branch: &'a str,
        status: &'a str,
    ) -> Self {
        Self {
            base_branch,
            kind: SessionSeedKind::Draft,
            model,
            project_git_branch: Some(base_branch),
            session_id,
            status,
            title: None,
        }
    }

    /// Build seed data for a stacked draft session.
    pub(crate) fn stacked_draft(
        session_id: &'a str,
        model: &'a str,
        worktree_path: &'a str,
        status: &'a str,
        parent_session_id: &'a str,
    ) -> Self {
        Self {
            base_branch: worktree_path,
            kind: SessionSeedKind::StackedDraft {
                parent_session_id,
                worktree_path,
            },
            model,
            project_git_branch: Some("main"),
            session_id,
            status,
            title: None,
        }
    }

    /// Return seed data that updates the inserted row title.
    pub(crate) fn with_title(mut self, title: &'a str) -> Self {
        self.title = Some(title);

        self
    }
}

/// Seed one session into the isolated E2E database.
///
/// Opens the database under [`BuilderEnv::agentty_root`], upserts and touches
/// the canonical test project, inserts the requested session row, and applies
/// an optional title update.
///
/// # Errors
///
/// Returns an error if runtime creation, project canonicalization, database
/// opening, project upsert/touch, or session insertion fails.
pub(crate) fn seed_session(
    env: &BuilderEnv,
    seed: SessionSeed<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = seed_runtime()?;

    runtime.block_on(async {
        let (database, project_id) =
            open_database_with_seeded_project(env, seed.project_git_branch).await?;
        insert_session_seed(&database, project_id, &seed).await
    })?;

    Ok(())
}

/// Create the current-thread Tokio runtime used by synchronous E2E seeders.
///
/// The E2E tests are synchronous `#[test]` functions, so database setup uses a
/// short-lived runtime that does not leak across PTY scenario execution.
///
/// # Errors
///
/// Returns an error if the Tokio runtime cannot be built.
pub(crate) fn seed_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Open the isolated E2E database and register the current test project.
///
/// The project path is canonicalized from [`BuilderEnv::workdir`], upserted
/// with `project_git_branch`, and marked as last opened so startup sees the
/// same active project as the seed data.
///
/// # Errors
///
/// Returns an error if path canonicalization, database opening, project upsert,
/// or last-opened persistence fails.
async fn open_database_with_seeded_project(
    env: &BuilderEnv,
    project_git_branch: Option<&str>,
) -> Result<(Database, i64), DbError> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let database = open_database(env).await?;
    let project_id = database
        .projects()
        .upsert_project(
            &canonical_workdir.to_string_lossy(),
            project_git_branch.map(str::to_string),
        )
        .await?;

    database
        .projects()
        .touch_project_last_opened(project_id)
        .await?;

    Ok((database, project_id))
}

/// Open the isolated E2E database for direct test data setup.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrated.
pub(crate) async fn open_database(env: &BuilderEnv) -> Result<Database, DbError> {
    let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);

    Database::open(&db_path).await
}

/// Insert one seeded session row and apply any row-level seed updates.
async fn insert_session_seed(
    database: &Database,
    project_id: i64,
    seed: &SessionSeed<'_>,
) -> Result<(), DbError> {
    match seed.kind {
        SessionSeedKind::Regular => {
            database
                .sessions()
                .insert_session(
                    seed.session_id,
                    seed.model,
                    seed.base_branch,
                    seed.status,
                    project_id,
                )
                .await?;
        }
        SessionSeedKind::Draft => {
            database
                .sessions()
                .insert_draft_session(
                    seed.session_id,
                    seed.model,
                    seed.base_branch,
                    seed.status,
                    project_id,
                )
                .await?;
        }
        SessionSeedKind::StackedDraft {
            parent_session_id,
            worktree_path,
        } => {
            database
                .sessions()
                .insert_stacked_draft_session(
                    seed.session_id,
                    seed.model,
                    worktree_path,
                    seed.status,
                    parent_session_id,
                    project_id,
                )
                .await?;
        }
    }

    if let Some(title) = seed.title {
        database
            .sessions()
            .update_session_title(seed.session_id, title)
            .await?;
    }

    Ok(())
}

/// Acquire the process-local E2E lock used by the standard Rust test harness.
///
/// This prevents same-process test threads from overlapping PTY scenarios.
/// Nextest launches each test in a separate process, so its cross-process
/// concurrency is governed by `.config/nextest.toml` instead.
pub(crate) fn acquire_e2e_test_lock() -> MutexGuard<'static, ()> {
    static E2E_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    E2E_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Return the feature GIF output directory as a pure path.
///
/// Derives the path from `CARGO_MANIFEST_DIR` →
/// `../../docs/site/static/features/`. This resolver intentionally does not
/// create the directory: runs without VHS installed never write a GIF, and
/// must not leave an empty directory behind. testty creates the directory
/// itself only when it actually records.
fn feature_output_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Path::new(manifest_dir).join("../../docs/site/static/features")
}

/// Environment variable that opts a [`FeatureTest`] run into GIF work.
///
/// Recognized values:
///
/// - `generate` / `generate-if-stale` → [`GifMode::GenerateIfStale`]
/// - `check` / `check-only` → [`GifMode::CheckOnly`]
/// - `force` / `always` / `always-generate` → [`GifMode::AlwaysGenerate`]
///
/// GIF work is **off** unless the variable names one of those modes and the
/// test declares a published feature page with [`FeatureTest::zola`], so an
/// ordinary local test run never shells out to VHS and never touches the docs
/// tree. CI opts in explicitly and is the only committer of GIFs.
///
/// Generate mode uses the frame hash as a cache key for VHS recording.
/// Check mode uses the same hash as a read-only freshness gate and fails when
/// an existing committed sidecar is stale or the GIF itself is missing.
pub(crate) const TESTTY_GIF_MODE_ENV_VAR: &str = "TESTTY_GIF_MODE";
/// Environment variable that pins the agentty wall clock for feature runs.
///
/// Mirrors `agentty::infra::clock::CLOCK_UNIX_ENV_VAR`, which is private to
/// the binary crate and therefore not importable from this integration test.
pub(crate) const PINNED_CLOCK_ENV_VAR: &str = "AGENTTY_CLOCK_UNIX";
/// Environment variable that pins the UTC offset paired with the feature clock.
///
/// Mirrors `agentty::infra::clock::CLOCK_UTC_OFFSET_SECONDS_ENV_VAR`, which is
/// private to the binary crate and therefore not importable from this test.
pub(crate) const PINNED_CLOCK_UTC_OFFSET_ENV_VAR: &str = "AGENTTY_CLOCK_UTC_OFFSET_SECONDS";
/// Environment variable that disables terminal color detection.
///
/// Feature GIF hashes include formatted terminal frames, including cell
/// styles. Pinning this avoids styling drift when a developer's shell exports
/// `NO_COLOR` but the Linux CI runner does not.
const NO_COLOR_ENV_VAR: &str = "NO_COLOR";
/// Value used to disable terminal color detection for feature runs.
const NO_COLOR_ENV_VALUE: &str = "1";
/// Wall-clock time every feature run is pinned to: `2026-07-01T00:00:00Z`.
///
/// Captured frames are hashed to decide whether a GIF needs re-recording, so
/// anything derived from the wall clock must not drift between runs. The
/// status bar rotates its `FYI:` hint once per minute, which alone would make
/// the same UI hash differently every minute.
///
/// This instant is a whole number of minutes past the epoch and its minute
/// count divides evenly by both rotating hint-set lengths, so every page shows
/// the first hint of its set. Any fixed value yields stable hashes; this one
/// also keeps the recorded hint predictable.
pub(crate) const PINNED_CLOCK_UNIX_SECONDS: i64 = 1_782_864_000;
/// UTC offset used by every feature run so host timezones cannot alter frames.
pub(crate) const PINNED_CLOCK_UTC_OFFSET_SECONDS: i64 = 0;
/// Prefix agentty prints in front of a session's generated worktree hash.
///
/// Mirrors `agentty::app::session::SESSION_BRANCH_PREFIX`, which is private to
/// the binary crate and therefore not importable from this integration test.
/// The footer bar shows it twice per session frame: once inside the worktree
/// path (`…/wt/4175e5af`) and once as the branch label (`wt/4175e5af`).
pub(crate) const SESSION_WORKTREE_PREFIX: &str = "wt/";
/// Longest run of session-UUID hex digits agentty can paint after `wt/`.
///
/// Mirrors the `session_id[..8]` truncation in `session_folder` and
/// `session_branch`. The UUID is generated per session, so those digits differ
/// on every run and must be redacted out of the freshness hash. Fewer than
/// eight reach the frame when the footer path runs past the right edge, which
/// is why the rule matches short runs too.
const SESSION_WORKTREE_MAX_HEX_LEN: usize = 8;
/// Placeholder substituted for a session's worktree hash while hashing frames.
pub(crate) const SESSION_WORKTREE_PLACEHOLDER: &str = "<session>";
/// Default PTY width used by feature tests unless a scenario requests a
/// wider responsive layout.
const DEFAULT_TERMINAL_COLS: u16 = 80;
/// Default PTY height used by feature tests unless a scenario requests a
/// taller responsive layout.
const DEFAULT_TERMINAL_ROWS: u16 = 24;

/// Resolve the GIF recording mode from [`TESTTY_GIF_MODE_ENV_VAR`].
///
/// Returns `None` when the variable is unset, which leaves GIF work off.
/// Otherwise delegates to [`parse_gif_mode`] for value parsing.
fn resolve_gif_mode() -> Option<GifMode> {
    let raw = std::env::var(TESTTY_GIF_MODE_ENV_VAR).ok()?;

    parse_gif_mode(&raw)
}

/// Pure parser that maps a raw `TESTTY_GIF_MODE` value to a [`GifMode`].
///
/// Returns `None` for empty input and unrecognized values, leaving GIF work
/// off rather than guessing at an expensive VHS run. Comparison is
/// case-insensitive and ignores surrounding whitespace.
fn parse_gif_mode(raw: &str) -> Option<GifMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "generate" | "generate-if-stale" => Some(GifMode::GenerateIfStale),
        "check" | "check-only" => Some(GifMode::CheckOnly),
        "force" | "always" | "always-generate" => Some(GifMode::AlwaysGenerate),
        _ => None,
    }
}

/// Return the GIF mode for a [`FeatureTest`] run.
///
/// Only tests that declare a Zola feature page participate in docs GIF
/// generation. Check mode is narrower: it validates only demos with a
/// committed page, GIF, or hash sidecar. Other [`FeatureTest`] uses are
/// regression tests that should still run under `TESTTY_GIF_MODE=check`, but
/// they do not have committed GIF artifacts to validate.
fn feature_gif_mode_for_run(gif_mode: Option<GifMode>, has_zola_page: bool) -> Option<GifMode> {
    gif_mode.filter(|_| has_zola_page)
}

/// Return whether this mode can mutate published artifacts.
fn gif_mode_records_artifacts(gif_mode: Option<GifMode>) -> bool {
    matches!(
        gif_mode,
        Some(GifMode::GenerateIfStale | GifMode::AlwaysGenerate)
    )
}

/// Return the redaction that hides a session's generated worktree hash.
///
/// Every [`FeatureTest`] applies it, because any frame showing a live session
/// carries a worktree name derived from a fresh UUID. Exposed so a feature test
/// can also assert the rule still matches the footer agentty actually paints.
pub(crate) fn session_worktree_redaction() -> Redaction {
    Redaction::hex_after(
        SESSION_WORKTREE_PREFIX,
        SESSION_WORKTREE_MAX_HEX_LEN,
        SESSION_WORKTREE_PLACEHOLDER,
    )
}

/// Return the redaction that hides the version painted in the header bar.
///
/// The header shows `Agentty v{CARGO_PKG_VERSION}`, so without this rule
/// every release bump would change every captured frame and stale every
/// committed GIF hash at once. The needle comes from this test binary's own
/// compile-time version, which matches the `agentty` binary under test
/// because both build from the same package.
fn agentty_version_redaction() -> Redaction {
    Redaction::literal(
        concat!("Agentty v", env!("CARGO_PKG_VERSION")),
        "Agentty <version>",
    )
}

/// Reconstruct a [`TerminalFrame`] from a [`ProofCapture`] so full cell-level
/// assertions (highlight, color, style) can be run against intermediate
/// captures.
pub(crate) fn frame_from_capture(capture: &ProofCapture) -> TerminalFrame {
    TerminalFrame::new(capture.cols, capture.rows, &capture.frame_bytes)
}

// ---------------------------------------------------------------------------
// Zola feature page generation
// ---------------------------------------------------------------------------

/// Return the Zola feature content directory path.
///
/// Derives the path from `CARGO_MANIFEST_DIR` →
/// `../../docs/site/content/features/`.
fn feature_content_dir_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    Path::new(manifest_dir).join("../../docs/site/content/features")
}

/// Return the Zola feature content directory, creating it if needed.
fn feature_content_dir() -> std::io::Result<PathBuf> {
    let content_dir = feature_content_dir_path();

    std::fs::create_dir_all(&content_dir)?;

    Ok(content_dir)
}

/// Return whether the run left a GIF on disk for this feature.
///
/// True for a freshly recorded GIF and for a cache hit against an already
/// committed one; false when VHS was unavailable or no output directory was
/// configured.
fn gif_exists_on_disk(gif_status: &GifStatus) -> bool {
    gif_status.gif_path().is_some_and(Path::is_file)
}

/// Metadata for generating a Zola feature content page.
///
/// When passed to [`FeatureTest::zola`], the test runner writes a minimal
/// `.md` frontmatter page to `docs/site/content/features/{name}.md` if the
/// file does not already exist.
pub(crate) struct ZolaFeaturePage {
    /// Human-readable title shown on the features page.
    pub(crate) title: String,
    /// Short description shown below the title.
    pub(crate) description: String,
    /// Ordering weight for the Zola features section (lower = first).
    pub(crate) weight: u32,
}

impl ZolaFeaturePage {
    /// Validate an existing page without creating content or directories.
    fn validate_existing(&self, name: &str) -> std::io::Result<()> {
        self.validate_existing_in(&feature_content_dir_path(), name)
    }

    /// Validate an existing page inside an explicit content directory.
    fn validate_existing_in(&self, content_dir: &Path, name: &str) -> std::io::Result<()> {
        let page_path = content_dir.join(format!("{name}.md"));
        let committed_content = std::fs::read_to_string(page_path)?;

        self.validate_content(name, &committed_content)
    }

    /// Publish or validate the page before feature artifacts can change.
    ///
    /// A newly published page is represented by a rollback guard so a later
    /// recording failure cannot leave it orphaned.
    fn begin_publication(&self, name: &str) -> std::io::Result<ZolaPagePublication> {
        let content_dir = feature_content_dir()?;

        self.begin_publication_in(&content_dir, name)
    }

    /// Publish or validate the page inside an explicit content directory.
    fn begin_publication_in(
        &self,
        content_dir: &Path,
        name: &str,
    ) -> std::io::Result<ZolaPagePublication> {
        let page_path = content_dir.join(format!("{name}.md"));
        let content = self.content(name);

        if page_path.exists() {
            let committed_content = std::fs::read_to_string(&page_path)?;
            self.validate_content(name, &committed_content)?;

            return Ok(ZolaPagePublication {
                committed: false,
                created: false,
                page_path,
            });
        }

        let staging_path = content_dir.join(format!(".{name}.recording.md"));
        std::fs::write(&staging_path, content)?;
        if let Err(error) = std::fs::rename(&staging_path, &page_path) {
            let _ = std::fs::remove_file(staging_path);

            return Err(error);
        }

        Ok(ZolaPagePublication {
            committed: false,
            created: true,
            page_path,
        })
    }

    /// Render the canonical page content for this feature.
    fn content(&self, name: &str) -> String {
        format!(
            "+++\ntitle = \"{title}\"\ndescription = \"{description}\"\nweight = \
             {weight}\n\n[extra]\ngif = \"{name}.gif\"\n+++\n",
            title = self.title,
            description = self.description,
            weight = self.weight,
        )
    }

    /// Validate committed metadata against the owning feature test.
    fn validate_content(&self, name: &str, content: &str) -> std::io::Result<()> {
        let expected_fields = [
            ("title", format!("\"{}\"", self.title)),
            ("description", format!("\"{}\"", self.description)),
            ("weight", self.weight.to_string()),
            ("gif", format!("\"{name}.gif\"")),
        ];

        for (field, expected_value) in expected_fields {
            let committed_value = content.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == field).then(|| value.trim())
            });
            if committed_value != Some(expected_value.as_str()) {
                return Err(std::io::Error::other(format!(
                    "feature page metadata mismatch for {name}: `{field}` is {committed_value:?}, \
                     expected `{expected_value}`"
                )));
            }
        }

        Ok(())
    }
}

/// Rollback guard for a Zola page published before feature artifacts.
struct ZolaPagePublication {
    committed: bool,
    created: bool,
    page_path: PathBuf,
}

impl ZolaPagePublication {
    /// Keep the published page after the artifact transaction succeeds.
    fn commit(mut self) {
        self.committed = true;
    }

    /// Remove a page created by this run while preserving existing pages.
    fn rollback(mut self) -> std::io::Result<()> {
        if !self.created {
            return Ok(());
        }

        let result = match std::fs::remove_file(&self.page_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        if result.is_ok() {
            self.committed = true;
        }

        result
    }
}

impl Drop for ZolaPagePublication {
    fn drop(&mut self) {
        if self.created && !self.committed {
            let _ = std::fs::remove_file(&self.page_path);
        }
    }
}

// ---------------------------------------------------------------------------
// FeatureTest builder
// ---------------------------------------------------------------------------

/// Declarative feature test builder for agentty E2E tests.
///
/// Owns the full test lifecycle: `TempDir` + [`BuilderEnv`] creation,
/// optional git init, scenario execution via [`FeatureDemo`], assertions,
/// GIF generation with hash caching, and optional Zola page creation.
///
/// # Example
///
/// ```ignore
/// #[test]
/// fn session_creation() {
///     FeatureTest::new("session_creation")
///         .with_git()
///         .zola("Session creation", "Start a new agent session.", 30)
///         .run(
///             |scenario| {
///                 scenario
///                     .compose(&common::wait_for_agentty_startup())
///                     .press_key("a")
///                     .press_key("Enter")
///                     .capture_labeled("prompt", "Prompt mode")
///             },
///             |frame, _report| {
///                 let full = Region::full(frame.cols(), frame.rows());
///                 assertion::assert_text_in_region(frame, "Enter", &full);
///             },
///         );
/// }
/// ```
pub(crate) struct FeatureTest {
    /// Extra child-process environment variables applied to PTY and VHS runs.
    child_env: Vec<(String, String)>,
    /// Whether PTY and VHS runs inherit the ambient system `PATH`.
    inherit_system_path: bool,
    /// Feature name used for GIF filename and Zola page filename.
    name: String,
    /// Optional environment setup hook that can seed database state or files
    /// before the PTY session starts.
    setup: Option<FeatureSetupHook>,
    /// Terminal column count used for the PTY proof run.
    terminal_cols: u16,
    /// Terminal row count used for the PTY proof run.
    terminal_rows: u16,
    /// Whether to initialize a git repository in the workdir.
    with_git: bool,
    /// Optional Zola page metadata for auto-generation.
    zola_page: Option<ZolaFeaturePage>,
}

/// Boxed setup hook used by [`FeatureTest`] before launching the PTY session.
type FeatureSetupHook = Box<dyn Fn(&BuilderEnv) -> Result<(), Box<dyn std::error::Error>>>;

/// Isolated fixture whose temporary root stays alive for a feature run.
struct PreparedFeatureEnvironment {
    env: BuilderEnv,
    temp: tempfile::TempDir,
}

impl PreparedFeatureEnvironment {
    /// Preserve the initialized fixture so the asserted proof can be reset
    /// before VHS replays the same scenario at the same paths.
    fn snapshot(&self) -> std::io::Result<FeatureEnvironmentSnapshot> {
        let temp = tempfile::TempDir::new()?;
        Self::copy_directory_contents(self.temp.path(), temp.path())?;

        Ok(FeatureEnvironmentSnapshot { temp })
    }

    /// Restore the initialized fixture after its semantic proof mutates it.
    fn restore(&self, snapshot: &FeatureEnvironmentSnapshot) -> std::io::Result<()> {
        Self::remove_directory_contents(self.temp.path())?;
        Self::copy_directory_contents(snapshot.temp.path(), self.temp.path())
    }

    /// Copy every fixture entry while preserving directories and symlinks.
    fn copy_directory_contents(source: &Path, destination: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                std::fs::create_dir(&destination_path)?;
                Self::copy_directory_contents(&source_path, &destination_path)?;
                std::fs::set_permissions(&destination_path, entry.metadata()?.permissions())?;
            } else if file_type.is_symlink() {
                symlink(std::fs::read_link(&source_path)?, &destination_path)?;
            } else if file_type.is_file() {
                std::fs::copy(&source_path, &destination_path)?;
            } else {
                return Err(std::io::Error::other(format!(
                    "unsupported fixture entry: {}",
                    source_path.display()
                )));
            }
        }

        Ok(())
    }

    /// Remove every mutable fixture entry without deleting its stable root.
    fn remove_directory_contents(directory: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(path)?;
            } else {
                std::fs::remove_file(path)?;
            }
        }

        Ok(())
    }
}

/// Immutable copy of one initialized feature fixture.
struct FeatureEnvironmentSnapshot {
    temp: tempfile::TempDir,
}

impl FeatureTest {
    /// Create a new feature test builder with the given name.
    ///
    /// The name is used as the GIF filename stem and Zola page filename.
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            child_env: vec![
                (
                    PINNED_CLOCK_ENV_VAR.to_string(),
                    PINNED_CLOCK_UNIX_SECONDS.to_string(),
                ),
                (
                    PINNED_CLOCK_UTC_OFFSET_ENV_VAR.to_string(),
                    PINNED_CLOCK_UTC_OFFSET_SECONDS.to_string(),
                ),
            ],
            inherit_system_path: true,
            name: name.into(),
            setup: None,
            terminal_cols: DEFAULT_TERMINAL_COLS,
            terminal_rows: DEFAULT_TERMINAL_ROWS,
            with_git: false,
            zola_page: None,
        }
    }

    /// Configure an environment setup hook that runs after optional git
    /// initialization and before the PTY session starts.
    pub(crate) fn setup(
        mut self,
        setup: impl Fn(&BuilderEnv) -> Result<(), Box<dyn std::error::Error>> + 'static,
    ) -> Self {
        self.setup = Some(Box::new(setup));

        self
    }

    /// Add an environment variable for the PTY session and VHS recording.
    pub(crate) fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.child_env.push((key.into(), value.into()));

        self
    }

    /// Enable git initialization in the test workdir.
    ///
    /// Required for tests that exercise worktree-dependent features like
    /// session creation.
    pub(crate) fn with_git(mut self) -> Self {
        self.with_git = true;

        self
    }

    /// Configure the PTY terminal dimensions used while running the feature
    /// proof.
    pub(crate) fn with_terminal_size(mut self, cols: u16, rows: u16) -> Self {
        self.terminal_cols = cols;
        self.terminal_rows = rows;

        self
    }

    /// Run the PTY proof and VHS tape with only deterministic stub
    /// executables on `PATH`.
    pub(crate) fn with_stub_only_path(mut self) -> Self {
        self.inherit_system_path = false;

        self
    }

    /// Configure Zola feature page auto-generation.
    ///
    /// When set, the test runner writes a minimal `.md` frontmatter page
    /// to `docs/site/content/features/{name}.md` if it does not already
    /// exist.
    pub(crate) fn zola(mut self, title: &str, description: &str, weight: u32) -> Self {
        self.zola_page = Some(ZolaFeaturePage {
            title: title.to_string(),
            description: description.to_string(),
            weight,
        });

        self
    }

    /// Run the feature test: build scenario, execute, assert, generate GIF.
    ///
    /// The `build_scenario` closure receives a fresh [`Scenario`] with the
    /// feature name and should return it after composing journeys and steps.
    /// The `assert` closure receives the final frame and proof report for
    /// semantic assertions. Generate modes invoke it again for the isolated
    /// recording proof before any GIF processing, so assertions must be
    /// deterministic and reusable.
    pub(crate) fn run(
        self,
        build_scenario: impl FnOnce(Scenario) -> Scenario,
        assert: impl Fn(&TerminalFrame, &ProofReport),
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _test_guard = acquire_e2e_test_lock();
        let scenario = build_scenario(Scenario::new(&self.name));
        let gif_mode = feature_gif_mode_for_run(resolve_gif_mode(), self.zola_page.is_some());
        let proof_environment = self.prepare_environment()?;
        let (proof_builder, proof_owned_pairs) = self.runtime_configuration(&proof_environment.env);
        let proof_env_pairs: Vec<(&str, &str)> = proof_owned_pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();

        // Generate modes execute assertions with GIF output disabled. A failed
        // assertion therefore cannot publish or invalidate documentation.
        let proof_gif_mode = (!gif_mode_records_artifacts(gif_mode))
            .then_some(gif_mode)
            .flatten();
        let result = self
            .feature_demo(proof_gif_mode)
            .run(
                &scenario,
                proof_builder,
                &cargo_bin("agentty"),
                &proof_env_pairs,
            )
            .map_err(|error| std::io::Error::other(format!("feature demo failed: {error}")))?;

        self.validate_gif_status(&result.gif_status)?;
        assert(&result.frame, &result.report);

        if matches!(gif_mode, Some(GifMode::CheckOnly))
            && let Some(zola_page) = &self.zola_page
        {
            zola_page.validate_existing(&self.name)?;
        }

        if let Some(gif_mode) = gif_mode
            .filter(|mode| matches!(mode, GifMode::GenerateIfStale | GifMode::AlwaysGenerate))
        {
            self.record_feature(&scenario, gif_mode, &assert)?;
        }

        Ok(())
    }

    /// Record one feature from an asserted proof after publishing its
    /// rollback-protected page.
    fn record_feature(
        &self,
        scenario: &Scenario,
        gif_mode: GifMode,
        assert: &impl Fn(&TerminalFrame, &ProofReport),
    ) -> Result<(), Box<dyn std::error::Error>> {
        let recording_environment = self.prepare_environment()?;
        let recording_snapshot = recording_environment.snapshot()?;
        let (recording_proof_builder, recording_owned_pairs) =
            self.runtime_configuration(&recording_environment.env);
        let recording_env_pairs: Vec<(&str, &str)> = recording_owned_pairs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let page_publication = self
            .zola_page
            .as_ref()
            .map(|page| page.begin_publication(&self.name))
            .transpose()?;
        let recording_result = self
            .feature_demo(Some(gif_mode))
            .run_with_assertion_and_recording_setup(
                scenario,
                recording_proof_builder,
                &cargo_bin("agentty"),
                &recording_env_pairs,
                assert,
                || {
                    recording_environment
                        .restore(&recording_snapshot)
                        .map_err(|error| {
                            VhsError::IoError(format!(
                                "failed to restore recording fixture: {error}"
                            ))
                        })
                },
            );
        let recording_result = match recording_result {
            Ok(recording_result) => recording_result,
            Err(error) => {
                let error: Box<dyn std::error::Error> =
                    std::io::Error::other(format!("feature recording failed: {error}")).into();

                return Err(Self::rollback_page_after_error(page_publication, error));
            }
        };

        if let Err(error) = self.validate_gif_status(&recording_result.gif_status) {
            return Err(Self::rollback_page_after_error(page_publication, error));
        }

        if let Some(page_publication) = page_publication {
            if gif_exists_on_disk(&recording_result.gif_status) {
                page_publication.commit();
            } else {
                page_publication.rollback()?;
            }
        }

        Ok(())
    }

    /// Roll back a newly published page and retain the original failure.
    fn rollback_page_after_error(
        page_publication: Option<ZolaPagePublication>,
        error: Box<dyn std::error::Error>,
    ) -> Box<dyn std::error::Error> {
        let Some(page_publication) = page_publication else {
            return error;
        };
        if let Err(rollback_error) = page_publication.rollback() {
            return std::io::Error::other(format!(
                "{error}; feature page rollback also failed: {rollback_error}"
            ))
            .into();
        }

        error
    }

    /// Prepare one pristine copy of the test fixture.
    fn prepare_environment(
        &self,
    ) -> Result<PreparedFeatureEnvironment, Box<dyn std::error::Error>> {
        let temp = tempfile::TempDir::new()?;
        let env = BuilderEnv::new(temp.path())?;

        if self.with_git {
            env.init_git()?;
        }
        if let Some(setup) = &self.setup {
            setup(&env)?;
        }

        Ok(PreparedFeatureEnvironment { env, temp })
    }

    /// Build matching PTY and VHS runtime configuration for one fixture.
    fn runtime_configuration(
        &self,
        env: &BuilderEnv,
    ) -> (PtySessionBuilder, Vec<(String, String)>) {
        let path_env = if self.inherit_system_path {
            env.path_with_stub_bin()
        } else {
            env.stub_only_path()
        };
        let mut builder = env.builder_with_path_and_size(
            path_env.clone(),
            self.terminal_cols,
            self.terminal_rows,
        );
        let mut env_pairs = env.as_vhs_env_pairs_with_path(path_env);

        for (key, value) in &self.child_env {
            builder = builder.env(key.clone(), value.clone());
            env_pairs.push((key.clone(), value.clone()));
        }

        (builder, env_pairs)
    }

    /// Build a demo with the shared deterministic redactions and GIF policy.
    fn feature_demo(&self, gif_mode: Option<GifMode>) -> FeatureDemo {
        let mut demo = FeatureDemo::new(&self.name)
            .redact(session_worktree_redaction())
            .redact(agentty_version_redaction());
        if let Some(gif_mode) = gif_mode {
            demo = demo.gif_output_dir(feature_output_dir()).gif_mode(gif_mode);
        }

        demo
    }

    /// Validates the GIF generation result for this feature.
    ///
    /// Every stale GIF is an error in check mode, including missing or invalid
    /// sidecars. A Zola declaration is a publication contract, so there is no
    /// legacy bypass for partially published artifact sets.
    fn validate_gif_status(
        &self,
        gif_status: &GifStatus,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Explicitly whitelist benign variants and fail on every error-like
        // variant. The wildcard is intentionally an error so a new variant
        // behind `#[non_exhaustive]` cannot be silently ignored.
        match gif_status {
            GifStatus::Generated(_)
            | GifStatus::CacheHit(_)
            | GifStatus::Fresh { .. }
            | GifStatus::VhsNotInstalled
            | GifStatus::NoOutputDir => Ok(()),
            GifStatus::Stale {
                gif_path,
                current,
                committed,
                committed_error,
            } => {
                let committed_error_detail = committed_error
                    .as_ref()
                    .map(|err| format!(", committed sidecar error: {err}"))
                    .unwrap_or_default();

                Err(std::io::Error::other(format!(
                    "Feature GIF is stale for {}: {} has current hash {current}, committed hash \
                     {committed:?}{committed_error_detail}",
                    self.name,
                    gif_path.display(),
                ))
                .into())
            }
            GifStatus::DirCreateFailed(err) => Err(std::io::Error::other(format!(
                "Feature GIF dir creation failed for {}: {err}",
                self.name
            ))
            .into()),
            GifStatus::TapeExecutionFailed(err) => Err(std::io::Error::other(format!(
                "VHS tape execution failed for {}: {err}",
                self.name
            ))
            .into()),
            other => Err(std::io::Error::other(format!(
                "Feature GIF generation returned an unrecognized status for {}: {other:?}. Update \
                 the FeatureTest harness to handle the new GifStatus variant.",
                self.name,
            ))
            .into()),
        }
    }
}

impl BuilderEnv {
    /// Initialize a git repository in the workdir so sessions can create
    /// worktrees.
    ///
    /// Sets up a `main` branch with an empty initial commit, a local bare
    /// `origin`, and an upstream tracking branch. The upstream prevents
    /// background sync from opening an unrelated failure dialog during a VHS
    /// replay.
    ///
    /// # Errors
    ///
    /// Returns an error if any git command fails.
    pub(crate) fn init_git(&self) -> std::io::Result<()> {
        let run = |args: &[&str]| -> std::io::Result<()> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.workdir)
                .output()?;
            if !output.status.success() {
                return Err(std::io::Error::other(format!(
                    "git {} failed: {}",
                    args.join(" "),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }

            Ok(())
        };
        run(&["init", "-b", "main"])?;
        run(&["config", "user.email", "test@test.com"])?;
        run(&["config", "user.name", "Test"])?;
        run(&["commit", "--allow-empty", "-m", "init"])?;

        let origin_path = self.home_dir.join("origin.git");
        let origin = origin_path.to_str().ok_or_else(|| {
            std::io::Error::other(format!("non-UTF-8 origin path: {}", origin_path.display()))
        })?;
        run(&["init", "--bare", origin])?;
        run(&["remote", "add", "origin", origin])?;
        run(&["push", "--set-upstream", "origin", "main"])
    }
}

// ---------------------------------------------------------------------------
// Agentty-specific Journey builders
// ---------------------------------------------------------------------------

/// Wait for agentty to start up and render a stable initial frame.
///
/// Waits for the initial TUI frame to appear and then settle briefly before
/// the scenario starts interacting with the app.
pub(crate) fn wait_for_agentty_startup() -> Journey {
    Journey::new("agentty_startup")
        .with_description("Wait for agentty startup and initial render")
        .step(Step::wait_for_text("Agentty", 30000))
        .step(Step::wait_for_stable_frame(300, 5000))
}

/// Switch to a tab by pressing `Tab` and waiting for the tab label text.
///
/// Useful for navigating forward through the tab bar one step at a time.
pub(crate) fn switch_to_tab(tab_name: &str) -> Journey {
    Journey::new(format!("switch_to_{tab_name}"))
        .with_description(format!("Press Tab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("Tab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Switch to a tab by pressing `BackTab` and waiting for stability.
///
/// Useful for navigating backward through the tab bar one step at a time.
pub(crate) fn switch_to_tab_reverse(tab_name: &str) -> Journey {
    Journey::new(format!("switch_back_to_{tab_name}"))
        .with_description(format!("Press BackTab and wait for '{tab_name}' to appear"))
        .step(Step::press_key("BackTab"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the quit confirmation dialog by pressing `q`.
///
/// Waits for the dialog to render with a stable frame.
pub(crate) fn open_quit_dialog() -> Journey {
    Journey::new("open_quit_dialog")
        .with_description("Press q and wait for quit confirmation dialog")
        .step(Step::press_key("q"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Open the help overlay by pressing `?`.
///
/// Waits for the overlay to render with a stable frame.
pub(crate) fn open_help_overlay() -> Journey {
    Journey::new("open_help_overlay")
        .with_description("Press ? and wait for help overlay")
        .step(Step::press_key("?"))
        .step(Step::wait_for_stable_frame(300, 3000))
}

/// Footer marker that only renders inside the in-session chat view.
///
/// `q: back` is the back-to-list help action exposed by the session view
/// footer. The sessions list view never renders this label, so it is a
/// reliable predicate target for the eventually waiter that detects when
/// the prompt-submit transition has completed.
const SESSION_VIEW_FOOTER_MARKER: &str = "q: back";

/// Footer marker that only renders on the sessions list view.
///
/// `new session` is the `a` shortcut label exposed by the sessions list
/// footer. The session chat view never renders this label, so it is a
/// reliable predicate target for the eventually waiter that detects when
/// the back-to-list transition has completed.
const SESSION_LIST_FOOTER_MARKER: &str = "new session";

/// Wait budget for a session view plus scenario-specific content.
///
/// Content-heavy views receive a longer deadline than ordinary view
/// transitions while still failing with the final structured frame context.
const SESSION_CONTENT_TRANSITION_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait budget for predicate-driven session-view transitions.
///
/// Five seconds covers slow CI workers without masking real regressions: a
/// faster settle short-circuits the waiter on the first matching poll, and
/// a true regression still surfaces a structured `AssertionFailure` instead
/// of an opaque over-sleep.
const SESSION_TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);

/// Polling cadence for the predicate-driven waiters above.
///
/// Fifty milliseconds keeps idle-CPU cost low while still finishing within
/// a single render frame on healthy hosts.
const SESSION_TRANSITION_POLL: Duration = Duration::from_millis(50);

/// Number of bottom rows scanned for footer help-action markers.
///
/// Both the sessions list page and the session view page render their
/// page-level help-action line two rows above the global worktree-status
/// footer bar (a one-cell page margin sits between them). Scanning the
/// bottom three rows therefore covers the page help row, the blank margin
/// row, and the global footer bar without reaching up into the chat
/// transcript or prompt input where agent output or user-typed text could
/// contain a matching substring.
const SESSION_TRANSITION_FOOTER_ROWS: u16 = 3;

/// Build an `eventually` step that succeeds once `marker` appears in the
/// live frame's bottom footer rows.
///
/// Wraps `assertion::match_text_in_region` against the bottom
/// `SESSION_TRANSITION_FOOTER_ROWS` rows so the waiter cannot be tricked by
/// prompt input or agent output that happens to contain the marker text, and
/// a timeout still surfaces a structured `AssertionFailure` carrying the
/// missing needle and current frame excerpt instead of a generic timeout
/// panic.
fn eventually_footer_text_visible(marker: &'static str) -> Step {
    Step::eventually(
        SESSION_TRANSITION_TIMEOUT,
        SESSION_TRANSITION_POLL,
        move |frame| {
            let rows = frame.rows();
            let height = SESSION_TRANSITION_FOOTER_ROWS.min(rows);
            let region = Region::new(0, rows.saturating_sub(height), frame.cols(), height);

            assertion::match_text_in_region(frame, marker, &region)
        },
    )
}

/// Build an `eventually` step that requires the session footer and every
/// scenario-specific marker in the same live frame.
fn eventually_session_view_texts_visible(expected_texts: Vec<String>) -> Step {
    Step::eventually(
        SESSION_CONTENT_TRANSITION_TIMEOUT,
        SESSION_TRANSITION_POLL,
        move |frame| match_session_view_texts(frame, &expected_texts),
    )
}

/// Matches one fully rendered session view and retains structured frame
/// diagnostics for the first missing marker.
fn match_session_view_texts(
    frame: &TerminalFrame,
    expected_texts: &[String],
) -> assertion::MatchResult {
    let rows = frame.rows();
    let footer_height = SESSION_TRANSITION_FOOTER_ROWS.min(rows);
    let footer_region = Region::new(
        0,
        rows.saturating_sub(footer_height),
        frame.cols(),
        footer_height,
    );
    assertion::match_text_in_region(frame, SESSION_VIEW_FOOTER_MARKER, &footer_region)?;

    let full = Region::full(frame.cols(), rows);
    for expected_text in expected_texts {
        assertion::match_text_in_region(frame, expected_text, &full)?;
    }

    Ok(())
}

/// Wait until the currently selected session has opened into chat view.
pub(crate) fn wait_for_session_view_footer() -> Step {
    eventually_footer_text_visible(SESSION_VIEW_FOOTER_MARKER)
}

/// Wait until the app has returned to the Sessions list view.
pub(crate) fn wait_for_session_list_footer() -> Step {
    eventually_footer_text_visible(SESSION_LIST_FOOTER_MARKER)
}

/// Press `Enter` and wait for the selected session view to render.
pub(crate) fn open_selected_session_view() -> Journey {
    Journey::new("open_selected_session_view")
        .with_description("Press Enter and wait for the session view footer")
        .step(Step::press_key("Enter"))
        .step(wait_for_session_view_footer())
}

/// Press `Enter` and wait for a fully rendered session view containing every
/// expected marker.
pub(crate) fn open_selected_session_view_with_texts(expected_texts: &[&str]) -> Journey {
    let expected_texts = expected_texts
        .iter()
        .map(|text| (*text).to_string())
        .collect();

    Journey::new("open_selected_session_view_with_texts")
        .with_description("Press Enter and wait for the expected session view content")
        .step(Step::press_key("Enter"))
        .step(eventually_session_view_texts_visible(expected_texts))
}

/// Press `q` and wait for the Sessions list to render.
pub(crate) fn return_to_session_list() -> Journey {
    Journey::new("return_to_session_list")
        .with_description("Press q and wait for the session list footer")
        .step(Step::press_key("q"))
        .step(wait_for_session_list_footer())
}

/// Create a session with a prompt, submit it, and return to the Sessions
/// list.
///
/// Presses `a` to open the creation selector, accepts the regular-session
/// default with `Enter`, types `"test"`, submits with `Enter` (which starts
/// the agent asynchronously while the session persists), and presses `q` from
/// the session view to return to the list.
///
/// Uses predicate-driven `eventually` waits keyed off footer markers because
/// the agent may produce continuous output after submit, so a stable-frame
/// wait can never settle.
///
/// Requires the Sessions tab to be active and a git-initialized workdir.
pub(crate) fn create_session_and_return_to_list() -> Journey {
    create_session_with_prompt_and_return_to_list("test")
}

/// Create a session with a caller-provided prompt, submit it, and return to
/// the Sessions list.
///
/// Uses predicate-driven `eventually` waits keyed off footer markers because
/// the agent may produce continuous output after submit, so a stable-frame
/// wait can never settle.
///
/// Requires the Sessions tab to be active and a git-initialized workdir.
pub(crate) fn create_session_with_prompt_and_return_to_list(prompt: &str) -> Journey {
    Journey::new("create_session")
        .with_description(format!(
            "Create regular session via a, type {prompt}, submit, return to list"
        ))
        .step(Step::press_key("a"))
        .step(Step::press_key("Enter"))
        .step(Step::wait_for_stable_frame(300, 5000))
        .step(Step::write_text(prompt))
        .step(Step::wait_for_text(prompt, 3000))
        .step(Step::press_key("Enter"))
        .step(wait_for_session_view_footer())
        .step(Step::press_key("q"))
        .step(wait_for_session_list_footer())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_zola_page() -> ZolaFeaturePage {
        ZolaFeaturePage {
            title: "Feature title".to_string(),
            description: "Feature description.".to_string(),
            weight: 42,
        }
    }

    #[test]
    fn match_session_view_texts_accepts_footer_and_all_markers() {
        // Arrange
        let frame = TerminalFrame::new(
            80,
            4,
            b"Campaign: Managed feature delivery\r\nremediation 1/3\r\n\r\nq: back",
        );
        let expected_texts = vec![
            "Campaign: Managed feature delivery".to_string(),
            "remediation 1/3".to_string(),
        ];

        // Act
        let result = match_session_view_texts(&frame, &expected_texts);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn match_session_view_texts_reports_missing_content_with_frame() {
        // Arrange
        let frame = TerminalFrame::new(
            80,
            4,
            b"Campaign: Managed feature delivery\r\nwaiting\r\n\r\nq: back",
        );
        let expected_texts = vec!["remediation 1/3".to_string()];

        // Act
        let failure = match_session_view_texts(&frame, &expected_texts)
            .expect_err("missing campaign content should fail");

        // Assert
        assert!(failure.message.contains("remediation 1/3"));
        assert!(
            failure
                .frame_excerpt
                .contains("Campaign: Managed feature delivery")
        );
    }

    #[test]
    fn match_session_view_texts_requires_session_footer() {
        // Arrange
        let frame = TerminalFrame::new(
            80,
            4,
            b"Campaign: Managed feature delivery\r\nremediation 1/3",
        );
        let expected_texts = vec!["remediation 1/3".to_string()];

        // Act
        let failure = match_session_view_texts(&frame, &expected_texts)
            .expect_err("campaign text outside a session view should fail");

        // Assert
        assert!(failure.message.contains(SESSION_VIEW_FOOTER_MARKER));
    }

    #[test]
    fn parse_gif_mode_recognizes_always_generate_aliases() {
        // Arrange / Act / Assert
        assert_eq!(parse_gif_mode("force"), Some(GifMode::AlwaysGenerate));
        assert_eq!(parse_gif_mode("always"), Some(GifMode::AlwaysGenerate));
        assert_eq!(
            parse_gif_mode("always-generate"),
            Some(GifMode::AlwaysGenerate),
        );
        assert_eq!(parse_gif_mode("Force"), Some(GifMode::AlwaysGenerate));
    }

    #[test]
    fn builder_env_keeps_painted_paths_under_home() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");

        // Act
        let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

        // Assert
        assert_eq!(env.agentty_root, env.home_dir.join(".agentty"));
        assert!(env.workdir.starts_with(&env.home_dir));
    }

    #[test]
    fn prepared_feature_environment_restores_the_asserted_fixture() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("create fixture root");
        let env = BuilderEnv::new(temp.path()).expect("create builder environment");
        let state_path = env.agentty_root.join("state.txt");
        let link_path = env.home_dir.join("state-link");
        std::fs::write(&state_path, "initialized").expect("write initialized state");
        symlink(&state_path, &link_path).expect("create fixture symlink");
        let prepared = PreparedFeatureEnvironment { env, temp };
        let snapshot = prepared.snapshot().expect("snapshot initialized fixture");
        std::fs::write(&state_path, "mutated").expect("mutate fixture state");
        std::fs::write(prepared.env.home_dir.join("unexpected.txt"), "unexpected")
            .expect("write unexpected state");

        // Act
        prepared.restore(&snapshot).expect("restore fixture");

        // Assert
        assert_eq!(
            std::fs::read_to_string(&state_path).expect("read restored state"),
            "initialized"
        );
        assert!(!prepared.env.home_dir.join("unexpected.txt").exists());
        assert!(
            std::fs::symlink_metadata(link_path)
                .expect("read restored symlink metadata")
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn prepared_feature_environment_rejects_unsupported_fixture_entries() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("create fixture root");
        let env = BuilderEnv::new(temp.path()).expect("create builder environment");
        let socket_path = env.home_dir.join("fixture.sock");
        let _socket = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("create unsupported socket entry");
        let prepared = PreparedFeatureEnvironment { env, temp };

        // Act
        let error = prepared
            .snapshot()
            .err()
            .expect("socket fixture should fail snapshot");

        // Assert
        assert!(error.to_string().contains("unsupported fixture entry"));
        assert!(error.to_string().contains("fixture.sock"));
    }

    #[test]
    fn builder_env_exports_no_color_to_vhs_recording() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
        let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

        // Act
        let environment = env.as_vhs_env_pairs();

        // Assert
        assert!(
            environment
                .iter()
                .any(|(key, value)| { key == NO_COLOR_ENV_VAR && value == NO_COLOR_ENV_VALUE }),
            "feature recording must disable color"
        );
        assert!(
            environment
                .iter()
                .any(|(key, value)| key == "PWD" && value == &env.workdir.to_string_lossy()),
            "feature recording must enter the PTY working directory"
        );
    }

    #[test]
    fn builder_env_stubs_every_supported_agent_cli() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");

        // Act
        let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

        // Assert
        for executable_name in STUB_AGENT_EXECUTABLES {
            assert!(
                env.stub_bin.join(executable_name).is_file(),
                "missing {executable_name} test stub",
            );
        }
    }

    #[test]
    fn builder_env_git_fixture_has_an_upstream() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
        let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");

        // Act
        env.init_git().expect("initialize git fixture");
        let output = std::process::Command::new("git")
            .args([
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ])
            .current_dir(&env.workdir)
            .output()
            .expect("inspect git upstream");

        // Assert
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "origin/main\n");
    }

    #[test]
    fn builder_env_keeps_antigravity_stub_supported_after_update() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("failed to create temporary directory");
        let env = BuilderEnv::new(temp.path()).expect("failed to create builder environment");
        let antigravity_path = env.stub_bin.join("agy");

        // Act
        let initial_output = std::process::Command::new(&antigravity_path)
            .arg("--version")
            .output()
            .expect("failed to execute Antigravity test stub");
        let update_status = std::process::Command::new(&antigravity_path)
            .arg("update")
            .status()
            .expect("failed to update Antigravity test stub");
        let updated_output = std::process::Command::new(&antigravity_path)
            .arg("--version")
            .output()
            .expect("failed to execute updated Antigravity test stub");

        // Assert
        assert!(initial_output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&initial_output.stdout),
            "agy 1.2.0\n"
        );
        assert!(update_status.success());
        assert!(updated_output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&updated_output.stdout),
            "agy 1.2.1\n"
        );
    }

    #[test]
    fn feature_test_pins_wall_clock_and_utc_offset() {
        // Arrange
        let expected_environment = [
            (
                PINNED_CLOCK_ENV_VAR.to_string(),
                PINNED_CLOCK_UNIX_SECONDS.to_string(),
            ),
            (
                PINNED_CLOCK_UTC_OFFSET_ENV_VAR.to_string(),
                PINNED_CLOCK_UTC_OFFSET_SECONDS.to_string(),
            ),
        ];

        // Act
        let feature_test = FeatureTest::new("deterministic_clock");

        // Assert
        for environment_entry in expected_environment {
            assert!(feature_test.child_env.contains(&environment_entry));
        }
    }

    #[test]
    fn parse_gif_mode_recognizes_generate_if_stale_aliases() {
        // Arrange / Act / Assert
        assert_eq!(parse_gif_mode("generate"), Some(GifMode::GenerateIfStale));
        assert_eq!(
            parse_gif_mode("  generate-if-stale  "),
            Some(GifMode::GenerateIfStale),
        );
    }

    #[test]
    fn parse_gif_mode_recognizes_check_only_aliases() {
        // Arrange / Act / Assert
        assert_eq!(parse_gif_mode("check"), Some(GifMode::CheckOnly));
        assert_eq!(parse_gif_mode("  check-only  "), Some(GifMode::CheckOnly));
        assert_eq!(parse_gif_mode("Check"), Some(GifMode::CheckOnly));
    }

    #[test]
    fn parse_gif_mode_leaves_recording_off_for_unrecognized_values() {
        // Arrange / Act / Assert
        assert_eq!(parse_gif_mode(""), None);
        assert_eq!(parse_gif_mode("nonsense"), None);
    }

    #[test]
    fn feature_gif_mode_for_run_keeps_zola_feature_mode() {
        // Arrange / Act / Assert
        assert_eq!(
            feature_gif_mode_for_run(Some(GifMode::CheckOnly), true),
            Some(GifMode::CheckOnly),
        );
    }

    #[test]
    fn feature_gif_mode_for_run_skips_regression_only_tests() {
        // Arrange / Act / Assert
        assert_eq!(
            feature_gif_mode_for_run(Some(GifMode::CheckOnly), false),
            None,
        );
    }

    #[test]
    fn gif_mode_records_artifacts_only_for_generate_modes() {
        // Arrange / Act / Assert
        assert!(!gif_mode_records_artifacts(None));
        assert!(!gif_mode_records_artifacts(Some(GifMode::CheckOnly)));
        assert!(gif_mode_records_artifacts(Some(GifMode::GenerateIfStale)));
        assert!(gif_mode_records_artifacts(Some(GifMode::AlwaysGenerate)));
    }

    #[test]
    fn zola_feature_page_accepts_matching_metadata() {
        // Arrange
        let page = test_zola_page();
        let content = page.content("feature_name");

        // Act
        let result = page.validate_content("feature_name", &content);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn zola_feature_page_rejects_metadata_drift() {
        // Arrange
        let page = test_zola_page();
        let content = page
            .content("feature_name")
            .replace("weight = 42", "weight = 7");

        // Act
        let error = page
            .validate_content("feature_name", &content)
            .expect_err("drifted metadata should fail");

        // Assert
        assert!(error.to_string().contains("`weight`"));
        assert!(error.to_string().contains("expected `42`"));
    }

    #[test]
    fn zola_feature_page_validates_existing_page_without_mutation() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let content = page.content("feature_name");
        std::fs::write(&page_path, &content).expect("write page");

        // Act
        page.validate_existing_in(temp.path(), "feature_name")
            .expect("validate page");

        // Assert
        assert_eq!(
            std::fs::read_to_string(page_path).expect("read page"),
            content
        );
    }

    #[test]
    fn zola_page_publication_rolls_back_new_page() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");
        assert!(page_path.is_file());

        // Act
        publication.rollback().expect("rollback page");

        // Assert
        assert!(!page_path.exists());
    }

    #[test]
    fn zola_page_publication_commit_keeps_new_page() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");

        // Act
        publication.commit();

        // Assert
        assert!(page_path.is_file());
    }

    #[test]
    fn zola_page_publication_drop_rolls_back_new_page() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");

        // Act
        drop(publication);

        // Assert
        assert!(!page_path.exists());
    }

    #[test]
    fn zola_page_publication_preserves_existing_page_on_rollback() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let content = page.content("feature_name");
        std::fs::write(&page_path, &content).expect("write page");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("validate page");

        // Act
        publication.rollback().expect("rollback existing page");

        // Assert
        assert_eq!(
            std::fs::read_to_string(page_path).expect("read page"),
            content
        );
    }

    #[test]
    fn zola_page_publication_rejects_drift_before_writing() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        std::fs::write(&page_path, page.content("feature_name").replace("42", "7"))
            .expect("write drifted page");

        // Act
        let error = page
            .begin_publication_in(temp.path(), "feature_name")
            .err()
            .expect("drifted page should fail");

        // Assert
        assert!(error.to_string().contains("`weight`"));
        assert!(!temp.path().join(".feature_name.recording.md").exists());
    }

    #[test]
    fn zola_page_publication_cleans_staging_file_when_rename_fails() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let staging_path = temp.path().join(".feature_name.recording.md");
        std::fs::create_dir(&page_path).expect("create conflicting page directory");

        // Act
        let result = page.begin_publication_in(temp.path(), "feature_name");

        // Assert
        assert!(result.is_err());
        assert!(!staging_path.exists());
        assert!(page_path.is_dir());
    }

    #[test]
    fn zola_page_publication_ignores_an_already_removed_new_page() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");
        std::fs::remove_file(page_path).expect("remove page");

        // Act
        let result = publication.rollback();

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn zola_page_publication_reports_rollback_failure() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");
        std::fs::remove_file(&page_path).expect("remove page");
        std::fs::create_dir(&page_path).expect("replace page with directory");

        // Act
        let result = publication.rollback();

        // Assert
        assert!(result.is_err());
        assert!(page_path.is_dir());
    }

    #[test]
    fn rollback_page_after_error_preserves_original_error() {
        // Arrange
        let error: Box<dyn std::error::Error> = std::io::Error::other("recording failed").into();

        // Act
        let returned_error = FeatureTest::rollback_page_after_error(None, error);

        // Assert
        assert_eq!(returned_error.to_string(), "recording failed");
    }

    #[test]
    fn rollback_page_after_error_removes_new_page() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");
        let error: Box<dyn std::error::Error> = std::io::Error::other("recording failed").into();

        // Act
        let returned_error = FeatureTest::rollback_page_after_error(Some(publication), error);

        // Assert
        assert_eq!(returned_error.to_string(), "recording failed");
        assert!(!page_path.exists());
    }

    #[test]
    fn rollback_page_after_error_combines_rollback_failure() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let page = test_zola_page();
        let page_path = temp.path().join("feature_name.md");
        let publication = page
            .begin_publication_in(temp.path(), "feature_name")
            .expect("publish page");
        std::fs::remove_file(&page_path).expect("remove page");
        std::fs::create_dir(&page_path).expect("replace page with directory");
        let error: Box<dyn std::error::Error> = std::io::Error::other("recording failed").into();

        // Act
        let returned_error = FeatureTest::rollback_page_after_error(Some(publication), error);

        // Assert
        assert!(returned_error.to_string().contains("recording failed"));
        assert!(
            returned_error
                .to_string()
                .contains("feature page rollback also failed")
        );
    }

    #[test]
    fn gif_exists_on_disk_reports_recorded_gif() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let gif_path = temp.path().join("feature.gif");
        std::fs::write(&gif_path, b"gif").expect("write gif");

        // Act / Assert
        assert!(gif_exists_on_disk(&GifStatus::Generated(gif_path.clone())));
        assert!(gif_exists_on_disk(&GifStatus::CacheHit(gif_path)));
    }

    #[test]
    fn gif_exists_on_disk_reports_skipped_and_missing_gifs() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let missing_gif_path = temp.path().join("absent.gif");

        // Act / Assert
        assert!(!gif_exists_on_disk(&GifStatus::Generated(missing_gif_path)));
        assert!(!gif_exists_on_disk(&GifStatus::VhsNotInstalled));
        assert!(!gif_exists_on_disk(&GifStatus::NoOutputDir));
    }

    #[test]
    fn validate_gif_status_accepts_fresh_check_result() {
        // Arrange
        let feature_test = FeatureTest::new("fresh_feature");
        let gif_status = GifStatus::Fresh {
            gif_path: PathBuf::from("docs/site/static/features/fresh_feature.gif"),
            hash: 42,
        };

        // Act
        let result = feature_test.validate_gif_status(&gif_status);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn validate_gif_status_rejects_stale_check_result() {
        // Arrange
        let feature_test = FeatureTest::new("stale_feature");
        let gif_status = GifStatus::Stale {
            gif_path: PathBuf::from("docs/site/static/features/stale_feature.gif"),
            current: 42,
            committed: Some(7),
            committed_error: None,
        };

        // Act
        let result = feature_test.validate_gif_status(&gif_status);

        // Assert
        let error = result.expect_err("stale GIF status should fail validation");
        let message = error.to_string();

        assert!(message.contains("Feature GIF is stale for stale_feature"));
        assert!(message.contains("current hash 42"));
        assert!(message.contains("committed hash Some(7)"));
    }

    #[test]
    fn validate_gif_status_rejects_existing_gif_without_sidecar() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let gif_path = temp.path().join("legacy_feature.gif");
        std::fs::write(&gif_path, b"gif").expect("write gif");

        let feature_test = FeatureTest::new("legacy_feature");
        let gif_status = GifStatus::Stale {
            gif_path,
            current: 42,
            committed: None,
            committed_error: None,
        };

        // Act
        let result = feature_test.validate_gif_status(&gif_status);

        // Assert
        let error = result.expect_err("missing sidecar should fail validation");
        assert!(error.to_string().contains("committed hash None"));
    }

    #[test]
    fn validate_gif_status_rejects_missing_gif_without_sidecar() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let feature_test = FeatureTest::new("missing_feature");
        let gif_status = GifStatus::Stale {
            gif_path: temp.path().join("missing_feature.gif"),
            current: 42,
            committed: None,
            committed_error: None,
        };

        // Act
        let result = feature_test.validate_gif_status(&gif_status);

        // Assert
        let error = result.expect_err("missing GIF should fail validation");
        let message = error.to_string();

        assert!(message.contains("Feature GIF is stale for missing_feature"));
        assert!(message.contains("committed hash None"));
    }

    #[test]
    fn validate_gif_status_rejects_invalid_sidecar_for_existing_gif() {
        // Arrange
        let temp = tempfile::TempDir::new().expect("temp dir");
        let gif_path = temp.path().join("invalid_sidecar.gif");
        std::fs::write(&gif_path, b"gif").expect("write gif");

        let feature_test = FeatureTest::new("invalid_sidecar");
        let gif_status = GifStatus::Stale {
            gif_path,
            current: 42,
            committed: None,
            committed_error: Some("failed to parse hash sidecar as u64".to_string()),
        };

        // Act
        let result = feature_test.validate_gif_status(&gif_status);

        // Assert
        let error = result.expect_err("invalid sidecar should fail validation");
        let message = error.to_string();

        assert!(message.contains("Feature GIF is stale for invalid_sidecar"));
        assert!(message.contains("committed hash None"));
        assert!(message.contains("committed sidecar error"));
    }
}
