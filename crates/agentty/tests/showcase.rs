//! Website feature GIF generation using VHS.
//!
//! These tests produce polished animated GIFs for the agentty website
//! (`docs/site/`) using the [VHS](https://github.com/charmbracelet/vhs)
//! terminal recorder. VHS records a real terminal session with proper
//! fonts, colors, and rendering fidelity. Output is rendered at high
//! resolution (3200x1600) for sharp display when downscaled in the
//! browser.
//!
//! The database is pre-seeded with realistic projects and sessions so the
//! UI looks populated and compelling.
//!
//! Each test corresponds to a feature demo on the website. When a test
//! changes, the corresponding GIF must be regenerated.
//!
//! # Prerequisites
//!
//! ```sh
//! brew install vhs
//! ```
//!
//! # Running
//!
//! Tests are marked `#[ignore]` so they do not run during regular
//! `cargo test`. Use `--ignored` to opt in:
//!
//! ```sh
//! cargo test -p agentty --test showcase -- --test-threads=1 --ignored --nocapture
//! ```
//!
//! Override output directory:
//!
//! ```sh
//! FEATURE_OUTPUT=/path/to/dir \
//!     cargo test -p agentty --test showcase -- --test-threads=1 --ignored --nocapture
//! ```

use std::fmt::Write;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::domain::session::SessionStats;
use agentty::domain::session_message::SessionMessageKind;
use assert_cmd::cargo::cargo_bin;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Result type used by ignored showcase-generation tests.
type ShowcaseResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Deterministic CLI versions rendered in the Projects dashboard.
const AGENT_CLI_STUBS: [(&str, &str); 3] = [
    ("agy", "agy 0.42.0"),
    ("claude", "claude 2.1.7"),
    ("codex", "codex-cli 0.114.0"),
];

/// One deterministic session row rendered by the showcase.
struct ShowcaseSessionSeed {
    id: &'static str,
    input_tokens: u64,
    model: &'static str,
    output_tokens: u64,
    size: &'static str,
    status: &'static str,
    title: &'static str,
}

/// Current-model session fleet shown across lifecycle groups.
const SHOWCASE_SESSIONS: [ShowcaseSessionSeed; 7] = [
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0001",
        input_tokens: 9_000,
        model: "claude-fable-5",
        output_tokens: 2_400,
        size: "M",
        status: "Review",
        title: "Fix auth token refresh on session expiry",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0002",
        input_tokens: 12_000,
        model: "gpt-5.6-sol",
        output_tokens: 3_900,
        size: "L",
        status: "InProgress",
        title: "Add pagination to the users list endpoint",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0003",
        input_tokens: 15_600,
        model: "gemini-3.1-pro-preview",
        output_tokens: 5_700,
        size: "S",
        status: "Review",
        title: "Refactor request handlers",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0004",
        input_tokens: 0,
        model: "claude-fable-5",
        output_tokens: 0,
        size: "M",
        status: "Question",
        title: "Clarify API error handling",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0005",
        input_tokens: 0,
        model: "gpt-5.6-terra",
        output_tokens: 0,
        size: "XS",
        status: "Done",
        title: "Add dark mode",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0006",
        input_tokens: 0,
        model: "claude-fable-5",
        output_tokens: 0,
        size: "S",
        status: "Done",
        title: "Update installation steps",
    },
    ShowcaseSessionSeed {
        id: "a1b2c3d4-0007",
        input_tokens: 0,
        model: "gpt-5.6-sol",
        output_tokens: 0,
        size: "L",
        status: "Queued",
        title: "Migrate configuration to TOML",
    },
];

/// Default feature output directory derived from the crate manifest location.
fn default_output_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    manifest_dir.join("../../docs/site/static/features")
}

/// Return the feature output directory, creating it if needed.
///
/// Uses `FEATURE_OUTPUT` if set, otherwise defaults to
/// `docs/site/static/features` relative to the workspace root.
fn feature_output_dir() -> ShowcaseResult<PathBuf> {
    let path = std::env::var("FEATURE_OUTPUT").map_or_else(|_| default_output_dir(), PathBuf::from);
    std::fs::create_dir_all(&path)?;

    Ok(path.canonicalize()?)
}

/// Build a VHS tape that launches agentty with the given environment.
///
/// The tape exports `AGENTTY_ROOT`, `HOME`, and the deterministic showcase
/// `PATH` in a hidden preamble, changes to `workdir`, visibly types `agentty`,
/// executes the provided VHS `steps` block, then quits. Output is rendered at
/// high resolution (3200x1600, font size 36) for sharp downscaling in the
/// browser.
fn build_tape(
    output_path: &Path,
    binary_path: &Path,
    agentty_root: &Path,
    workdir: &Path,
    steps: &str,
) -> String {
    let mut tape = String::new();
    let binary_dir = binary_path.parent().unwrap_or(binary_path);
    let home_dir = agentty_root.parent().unwrap_or(agentty_root);
    let stub_bin = home_dir.join(".stub-bin");
    let path_env = std::env::join_paths([
        stub_bin,
        binary_dir.to_path_buf(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .map_or_else(
        |_| binary_dir.display().to_string(),
        |value| value.to_string_lossy().into_owned(),
    );

    // Header settings — high resolution for sharp downscaling.
    let _ = writeln!(tape, "Set Shell \"bash\"");
    let _ = writeln!(tape, "Set FontSize 36");
    let _ = writeln!(tape, "Set Width 3200");
    let _ = writeln!(tape, "Set Height 1600");
    let _ = writeln!(tape, "Set Padding 0");
    let _ = writeln!(tape, "Set Framerate 30");
    let _ = writeln!(tape, "Set TypingSpeed 0");
    let _ = writeln!(tape, "Set Theme \"OneDark\"");
    let _ = writeln!(tape);
    let _ = writeln!(tape, "Output \"{}\"", output_path.display());
    let _ = writeln!(tape);

    // Hidden environment setup.
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(
        tape,
        "Type \"export AGENTTY_ROOT='{}'\"",
        agentty_root.display()
    );
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"export HOME='{}'\"", home_dir.display());
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"export PATH='{path_env}'\"");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"unset NO_COLOR\"");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"cd '{}'\"", workdir.display());
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Type \"clear\"");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape, "Sleep 200ms");
    let _ = writeln!(tape, "Set TypingSpeed 55ms");
    let _ = writeln!(tape, "Show");
    let _ = writeln!(tape);

    // Launch the binary visibly, matching the marketing demo's opening.
    let _ = writeln!(tape, "Sleep 600ms");
    let _ = writeln!(tape, "Type \"agentty\"");
    let _ = writeln!(tape, "Sleep 400ms");
    let _ = writeln!(tape, "Enter");
    let _ = writeln!(tape);

    // Scenario steps.
    let _ = write!(tape, "{steps}");
    let _ = writeln!(tape);

    // Hidden teardown.
    let _ = writeln!(tape, "Hide");
    let _ = writeln!(tape, "Type \"q\"");
    let _ = writeln!(tape, "Sleep 1s");

    tape
}

#[test]
fn build_tape_visibly_launches_colored_agentty() {
    // Arrange
    let output_path = Path::new("/tmp/sessions.gif");
    let binary_path = Path::new("/tmp/target/debug/agentty");
    let agentty_root = Path::new("/tmp/home/.agentty");
    let workdir = Path::new("/tmp/home/opencloudtool");

    // Act
    let tape = build_tape(
        output_path,
        binary_path,
        agentty_root,
        workdir,
        "Sleep 1s\n",
    );

    // Assert
    assert!(tape.contains("Set TypingSpeed 0"));
    assert!(tape.contains("Type \"export PATH='/tmp/home/.stub-bin:"));
    assert!(tape.contains("Type \"unset NO_COLOR\""));
    assert!(tape.contains(
        "Set TypingSpeed 55ms\nShow\n\nSleep 600ms\nType \"agentty\"\nSleep 400ms\nEnter"
    ));
}

/// Execute a VHS tape and verify the output was produced.
///
/// Writes the tape to a temporary file, runs `vhs`, and asserts the
/// output file exists at `output_path` on completion.
fn execute_tape(tape_content: &str, name: &str, output_path: &Path) -> ShowcaseResult {
    let tape_path = std::env::temp_dir().join(format!("{name}.tape"));
    std::fs::write(&tape_path, tape_content)?;

    let output = Command::new("vhs").arg(&tape_path).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(std::io::Error::other(format!("VHS execution failed: {stderr}")).into());
    }

    assert!(
        output_path.exists(),
        "VHS did not produce output at {}",
        output_path.display()
    );

    Ok(())
}

/// Seed the database with realistic projects and sessions.
///
/// Opens the `SQLite` database, runs migrations, and inserts sample data
/// that makes the UI look populated and visually compelling.
async fn seed_database(agentty_root: &Path, workdir: &Path) -> ShowcaseResult {
    let db_path = agentty_root.join(DB_DIR).join(DB_FILE);
    let database = Database::open(&db_path).await?;

    // Register the project (mirrors what agentty does on startup).
    let project_id = database
        .projects()
        .upsert_project(&workdir.to_string_lossy(), Some("main".to_string()))
        .await?;

    // Touch last-opened so the project sorts to the top.
    database
        .projects()
        .touch_project_last_opened(project_id)
        .await?;

    seed_showcase_sessions(&database, project_id, agentty_root).await?;

    let now_seconds = unix_timestamp_now();
    let activity_project_id = database
        .projects()
        .upsert_project(
            &agentty_root.join("activity-history").to_string_lossy(),
            Some("main".to_string()),
        )
        .await?;
    seed_dashboard_activity(&database, activity_project_id, now_seconds).await?;

    Ok(())
}

/// Inserts the visible showcase fleet and starts its running-session timer.
async fn seed_showcase_sessions(
    database: &Database,
    project_id: i64,
    agentty_root: &Path,
) -> ShowcaseResult {
    let wt_dir = agentty_root.join("wt");

    for session in &SHOWCASE_SESSIONS {
        database
            .sessions()
            .insert_session(
                session.id,
                session.model,
                "main",
                session.status,
                project_id,
            )
            .await?;

        database
            .sessions()
            .update_session_title(session.id, session.title)
            .await?;

        database
            .sessions()
            .update_session_diff_stats(10, 5, true, session.id, session.size)
            .await?;
        database
            .sessions()
            .update_session_stats(
                session.id,
                &SessionStats {
                    input_tokens: session.input_tokens,
                    output_tokens: session.output_tokens,
                    ..SessionStats::default()
                },
            )
            .await?;

        // Create the worktree directory so the session shows a valid folder.
        let session_wt = wt_dir.join(&session.id[..8]);
        std::fs::create_dir_all(&session_wt)?;
    }

    database
        .sessions()
        .update_session_status_with_timing_at(
            "a1b2c3d4-0002",
            "InProgress",
            unix_timestamp_now().saturating_sub(4),
        )
        .await?;
    database
        .sessions()
        .append_session_message(
            "a1b2c3d4-0001",
            SessionMessageKind::UserPrompt,
            "Fix the auth token refresh race that logs users out when sessions expire.",
        )
        .await?;
    database
        .sessions()
        .append_session_message(
            "a1b2c3d4-0001",
            SessionMessageKind::AssistantAnswer,
            "Implemented a single-flight token refresh path and added regression coverage for \
             concurrent expiry.\n\n- Reuses one in-flight refresh across requests\n- Retries the \
             original request after success\n- Clears credentials only when refresh fails\n\nAll \
             authentication tests pass.",
        )
        .await?;
    database
        .sessions()
        .append_session_message(
            "a1b2c3d4-0001",
            SessionMessageKind::WorkflowNotice,
            "\n[Commit] committed with hash `a1b2c3d`\n",
        )
        .await?;

    Ok(())
}

/// Seeds recent and historical session-creation events for the activity map
/// and Work Pace summaries without adding rows to the visible showcase
/// project.
async fn seed_dashboard_activity(
    database: &Database,
    activity_project_id: i64,
    now_seconds: i64,
) -> ShowcaseResult {
    let day_seconds = 86_400_i64;
    let active_day_offsets = [0_i64, 1, 2, 3, 5, 8, 9, 12, 16, 21, 27, 34, 41, 55, 69];

    for (index, day_offset) in active_day_offsets.into_iter().enumerate() {
        let events_for_day = index % 4 + 1;
        for event_index in 0..events_for_day {
            let timestamp = now_seconds
                .saturating_sub(day_offset.saturating_mul(day_seconds))
                .saturating_sub(i64::try_from(event_index).unwrap_or_default() * 1_800);
            let session_id = format!("showcase-activity-{day_offset:02}-{event_index:02}");
            database
                .sessions()
                .insert_session(
                    &session_id,
                    "gpt-5.6-sol",
                    "main",
                    "Done",
                    activity_project_id,
                )
                .await?;
            database
                .activity()
                .insert_session_creation_activity_at(&session_id, timestamp)
                .await?;
        }
    }

    Ok(())
}

/// Returns the current Unix timestamp with a saturating signed conversion.
fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

#[tokio::test]
async fn seed_database_populates_showcase_dashboard_and_timer() -> ShowcaseResult {
    // Arrange
    let temp = tempfile::TempDir::new()?;
    let agentty_root = temp.path().join(".agentty");
    let workdir = temp.path().join("opencloudtool");
    std::fs::create_dir_all(&agentty_root)?;
    std::fs::create_dir_all(&workdir)?;

    // Act
    seed_database(&agentty_root, &workdir).await?;
    let database = Database::open(&agentty_root.join(DB_DIR).join(DB_FILE)).await?;
    let projects = database.projects().load_projects_with_stats().await?;
    let showcase_project = projects
        .iter()
        .find(|project| project.path == workdir.to_string_lossy())
        .ok_or("missing showcase project")?;
    let activity_timestamps = database
        .activity()
        .load_session_activity_timestamps()
        .await?;
    let sessions = database
        .sessions()
        .load_sessions_for_project(showcase_project.id)
        .await?;
    let running_session = sessions
        .iter()
        .find(|session| session.id == "a1b2c3d4-0002")
        .ok_or("missing running showcase session")?;
    let review_messages = database
        .sessions()
        .load_session_messages("a1b2c3d4-0001")
        .await?;

    // Assert
    assert_eq!(showcase_project.active_session_count, 4);
    assert_eq!(showcase_project.input_tokens, 36_600);
    assert_eq!(showcase_project.output_tokens, 12_000);
    assert_eq!(showcase_project.session_count, 7);
    assert_eq!(activity_timestamps.len(), 36);
    assert!(running_session.in_progress_started_at.is_some());
    assert_eq!(review_messages.len(), 3);
    assert_eq!(
        review_messages[1].kind,
        SessionMessageKind::AssistantAnswer.as_str()
    );
    assert!(
        review_messages[1]
            .content
            .contains("single-flight token refresh")
    );
    assert_eq!(
        review_messages[2].kind,
        SessionMessageKind::WorkflowNotice.as_str()
    );
    assert!(review_messages[2].content.contains("committed with hash"));
    Ok(())
}

// ── Feature Scenarios ────────────────────────────────────────────────────

/// Set up an isolated feature demo environment with a seeded database and Git
/// repository.
///
/// Returns `(agentty_root, workdir)` with canonicalized paths so the
/// seeded project path matches what the binary resolves (critical on
/// macOS where `/var/folders` is a symlink to `/private/var/folders`).
async fn setup_feature_env(temp: &tempfile::TempDir) -> ShowcaseResult<(PathBuf, PathBuf)> {
    let home_dir = temp.path().join("home");
    let agentty_root = home_dir.join(".agentty");
    let stub_bin = home_dir.join(".stub-bin");
    let workdir = home_dir.join("opencloudtool");
    std::fs::create_dir_all(&home_dir)?;
    std::fs::create_dir_all(&agentty_root)?;
    std::fs::create_dir_all(&stub_bin)?;
    std::fs::create_dir_all(&workdir)?;
    for (executable_name, version_output) in AGENT_CLI_STUBS {
        let stub_path = stub_bin.join(executable_name);
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 0; fi\nif [ \"$1\" = \"--version\" \
             ]; then printf '%s\\n' '{version_output}'; exit 0; fi\nexit 1\n"
        );
        std::fs::write(&stub_path, script)?;
        #[cfg(unix)]
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755))?;
    }
    let git_init = {
        let workdir = workdir.clone();
        tokio::task::spawn_blocking(move || {
            Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(workdir)
                .status()
        })
        .await??
    };
    if !git_init.success() {
        return Err(std::io::Error::other("failed to initialize showcase git repository").into());
    }

    // Canonicalize so the seeded path matches the binary's resolved CWD.
    let agentty_root = agentty_root.canonicalize()?;
    let workdir = workdir.canonicalize()?;

    seed_database(&agentty_root, &workdir).await?;

    Ok((agentty_root, workdir))
}

/// Generate a feature GIF of the Sessions tab with populated data.
///
/// Shows the session list with sessions in various statuses (`Review`,
/// `InProgress`, `Question`, `Done`, `Queued`) to demonstrate the core
/// workflow and UI layout.
#[tokio::test]
#[ignore = "requires VHS and a renderable terminal — run explicitly with --ignored"]
async fn feature_sessions_tab() -> ShowcaseResult {
    // Arrange
    let temp = tempfile::TempDir::new()?;
    let (agentty_root, workdir) = setup_feature_env(&temp).await?;
    let binary_path = cargo_bin("agentty");
    let gif_path = feature_output_dir()?.join("sessions.gif");

    let steps = "\
Sleep 2400ms
Sleep 200ms
Enter
Sleep 2200ms
Type \"j\"
Sleep 900ms
Type \"k\"
Sleep 900ms
Enter
Sleep 2800ms
Type \"q\"
Sleep 1800ms
";

    let tape = build_tape(&gif_path, &binary_path, &agentty_root, &workdir, steps);

    // Act + Assert
    execute_tape(&tape, "feature_sessions", &gif_path)
}

/// Generate a feature GIF cycling through all tabs.
///
/// Shows each major view — Projects, Sessions, and Settings — to
/// give a complete overview of the agentty interface.
#[tokio::test]
#[ignore = "requires VHS and a renderable terminal — run explicitly with --ignored"]
async fn feature_tab_tour() -> ShowcaseResult {
    // Arrange
    let temp = tempfile::TempDir::new()?;
    let (agentty_root, workdir) = setup_feature_env(&temp).await?;
    let binary_path = cargo_bin("agentty");
    let gif_path = feature_output_dir()?.join("tab_tour.gif");

    let steps = "\
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
Tab
Sleep 2s
";

    let tape = build_tape(&gif_path, &binary_path, &agentty_root, &workdir, steps);

    // Act + Assert
    execute_tape(&tape, "feature_tab_tour", &gif_path)
}
