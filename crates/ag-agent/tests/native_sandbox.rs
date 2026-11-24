//! Agentty's command grants under the real native sandbox: Bubblewrap on
//! Linux, Seatbelt on macOS. Missing isolation fails the test.
//!
//! The binary is its own test harness so that `main` can call
//! `dispatch_sandbox_helper` before anything else, as Agentty's `main`
//! does; the harness then starts this executable as the sandbox helper. It
//! answers the `cargo nextest` listing protocol, runs its one test when
//! selected, and fails on any other argument or a filter that selects
//! nothing, so it never passes without running.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ag_agent::{NativeHarnessConfig, create_agent_channel, dispatch_sandbox_helper};
use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, ContextCompaction, ExecutionPolicy, PermissionMode,
    ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnEvent, TurnNotice, TurnRequest,
};
use ag_git::{GitClient, RealGitClient};
use ag_harness::bash::Enforcement;
use ag_harness::tool::ToolDefinition;
use ag_protocol::TurnPrompt;
use ag_session::AgentKind;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::mpsc;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Result of one fallible test step.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The only test this harness runs.
const TEST_NAME: &str = "session_commands_commit_in_a_linked_worktree_and_stay_confined";
/// Harness model served by the scripted provider.
const MODEL: &str = "muse-spark-1.3";
/// Session whose harness history the command tries to read.
const SESSION_ID: &str = "session-sandbox";
/// Session branch checked out in the worktree, nested like Agentty's own so
/// that Linux, which grants the directory holding it, keeps `main` read-only.
const SESSION_BRANCH: &str = "wt/session";
/// Stored API key file, under the Agentty root, the command tries to read.
const API_KEY_FILE: &str = "auth/api_keys.json";

fn main() -> ExitCode {
    if let Some(helper) = dispatch_sandbox_helper() {
        return helper;
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let outcome = match Invocation::parse(&arguments) {
        Ok(Invocation::List { ignored }) => {
            // This harness has no ignored tests.
            let listing = if ignored {
                String::new()
            } else {
                format!("{TEST_NAME}: test\n")
            };

            io::stdout()
                .write_all(listing.as_bytes())
                .map_err(Box::<dyn Error>::from)
        }
        Ok(Invocation::Run) => run(),
        Err(error) => Err(error.into()),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{TEST_NAME}: {error}");

            ExitCode::FAILURE
        }
    }
}

/// What the test runner asked this harness to do.
enum Invocation {
    /// List the tests, or the ignored ones, as `cargo nextest` expects.
    List {
        /// Whether only ignored tests are listed.
        ignored: bool,
    },
    /// Run the test.
    Run,
}

impl Invocation {
    /// Parses the runner's arguments: `--list --format terse [--ignored]`,
    /// or an optional name filter with `--exact` and `--nocapture`, as
    /// `cargo nextest` and plain `cargo test` pass them.
    ///
    /// # Errors
    /// Returns an error for any other argument, a second filter, or a
    /// filter that does not select the test.
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut list = false;
        let mut ignored = false;
        let mut exact = false;
        let mut filter = None;
        let mut remaining = arguments.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--list" => list = true,
                "--ignored" => ignored = true,
                "--exact" => exact = true,
                "--nocapture" => {}
                "--format" if remaining.next().map(String::as_str) == Some("terse") => {}
                name if !name.starts_with('-') && filter.is_none() => filter = Some(name),
                other => return Err(format!("unsupported argument `{other}`")),
            }
        }
        if list {
            return Ok(Self::List { ignored });
        }
        if ignored {
            return Err("this harness has no ignored tests to run".to_string());
        }
        let selected = filter.is_none_or(|filter| {
            if exact {
                filter == TEST_NAME
            } else {
                TEST_NAME.contains(filter)
            }
        });
        if !selected {
            return Err(format!(
                "the filter selects no test; the only test is `{TEST_NAME}`"
            ));
        }

        Ok(Self::Run)
    }
}

/// Runs the test, starting its runtime only after `main` dispatched.
#[tokio::main]
async fn run() -> TestResult {
    session_commands_commit_in_a_linked_worktree_and_stay_confined().await
}

/// Runs a scripted session turn whose one command commits in a linked
/// worktree, uses its temporary directory, builds a repository there and
/// moves it into the worktree, and tries to write outside the worktree and
/// to read the harness history.
async fn session_commands_commit_in_a_linked_worktree_and_stay_confined() -> TestResult {
    // Arrange
    let fixture = Fixture::new()?;
    let server = MockServer::start().await;
    respond(&server, r#""tool_call_id":"call-bash""#, answer("Done"), 1).await;
    respond(&server, "Commit notes", bash_call(&fixture.command()), 2).await;
    let config = fixture.config(&server);
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn(
            SESSION_ID.to_string(),
            turn_request(&fixture.worktree, "Commit notes"),
            events_tx,
        )
        .await;

    // Assert
    let mut notices = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        if let TurnEvent::Notice(notice) = event {
            notices.push(notice);
        }
    }
    assert!(
        !notices
            .iter()
            .any(|notice| matches!(notice, TurnNotice::SandboxUnavailable { .. })),
        "the native sandbox must run here: {notices:?}"
    );
    assert_eq!(result?.assistant_message.answer, "Done");
    let bodies = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect::<Vec<_>>();
    let sandboxed = json!(ToolDefinition::bash(Enforcement::FileSystem).description()).to_string();
    assert!(
        bodies.first().is_some_and(|body| body.contains(&sandboxed)),
        "the bash tool must state the filesystem sandbox"
    );
    let bash_result = bodies
        .iter()
        .find(|body| body.contains(r#""tool_call_id":"call-bash""#))
        .ok_or("bash result should reach the model")?;
    // The command commits, then the moves of the base branch, the edit of
    // `packed-refs`, the write outside, and the reads of the harness history
    // and the application database fail, so only the first marker reaches
    // standard output.
    assert!(
        bash_result.contains(r#"\"stdout\":\"committed\\n\""#),
        "{bash_result}"
    );
    assert!(
        notices.contains(&TurnNotice::LikelySandboxDenial),
        "the denied write must be reported: {notices:?}"
    );
    assert!(
        notices.contains(&TurnNotice::SandboxAvailable),
        "the sandboxed turn must report it: {notices:?}"
    );
    fixture.assert_effects()?;
    fixture.assert_planted_repository_ignored().await
}

/// A repository with a linked session worktree, a directory outside it, the
/// harness data root, and an Agentty root holding the application database.
struct Fixture {
    agentty_root: TempDir,
    data_root: TempDir,
    outside: TempDir,
    repository: TempDir,
    worktree: PathBuf,
    _worktrees: TempDir,
}

impl Fixture {
    /// Creates the repository with one commit and its linked worktree on the
    /// session branch.
    fn new() -> TestResult<Self> {
        let repository = tempfile::tempdir()?;
        let worktrees = tempfile::tempdir()?;
        let worktree = worktrees.path().join("session");
        git(repository.path(), &["init", "-q", "-b", "main"])?;
        std::fs::write(repository.path().join("README.md"), "base\n")?;
        git(repository.path(), &["add", "README.md"])?;
        git(repository.path(), &["commit", "-qm", "Base"])?;
        let worktree_path = worktree.display().to_string();
        git(
            repository.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                SESSION_BRANCH,
                &worktree_path,
            ],
        )?;

        let agentty_root = tempfile::tempdir()?;
        std::fs::create_dir(agentty_root.path().join("db"))?;
        std::fs::write(agentty_root.path().join("db/agentty.db"), "transcripts")?;
        std::fs::create_dir(agentty_root.path().join("auth"))?;
        std::fs::write(
            agentty_root.path().join(API_KEY_FILE),
            r#"{"providers":{"muse":{"api_key":"muse-key"}},"version":1}"#,
        )?;

        let fixture = Self {
            agentty_root,
            data_root: tempfile::tempdir()?,
            outside: tempfile::tempdir()?,
            repository,
            worktree,
            _worktrees: worktrees,
        };
        std::fs::write(
            fixture.fsmonitor(),
            format!(
                "#!/bin/sh\ntouch '{}'\nexit 1\n",
                fixture.marker().display()
            ),
        )?;
        std::fs::set_permissions(fixture.fsmonitor(), std::fs::Permissions::from_mode(0o755))?;

        Ok(fixture)
    }

    /// Returns the command the model runs: a commit, a temporary file, moves
    /// of the base branch and edits of `packed-refs`, a write outside the
    /// worktree, and reads of the harness history and of Agentty's
    /// application database and stored API keys.
    fn command(&self) -> String {
        format!(
            "export GIT_AUTHOR_NAME=Agentty GIT_AUTHOR_EMAIL=agentty@example.com \
             GIT_COMMITTER_NAME=Agentty GIT_COMMITTER_EMAIL=agentty@example.com; printf \
             'sandboxed\\n' > notes.txt && git add notes.txt && git commit -qm 'Add notes' && \
             touch \"$TMPDIR/scratch\" && echo committed; git init -q -b main \"$TMPDIR/planted\" \
             && git -C \"$TMPDIR/planted\" commit -q --allow-empty -m planted && git -C \
             \"$TMPDIR/planted\" config core.fsmonitor '{}' && mv \"$TMPDIR/planted\" planted; \
             git update-ref refs/heads/main HEAD 2>/dev/null && echo moved-main; printf x >> \
             \"$(git rev-parse --path-format=absolute --git-common-dir)/packed-refs\" && echo \
             edited-packed-refs; touch '{}' && echo escaped; cat '{}' > /dev/null && echo \
             read-history; cat '{}' > /dev/null && echo read-database; cat '{}' > /dev/null && \
             echo read-keys",
            self.fsmonitor().display(),
            self.escape().display(),
            self.data_root
                .path()
                .join(SESSION_ID)
                .join("harness.db")
                .display(),
            self.agentty_root.path().join("db/agentty.db").display(),
            self.agentty_root.path().join(API_KEY_FILE).display(),
        )
    }

    /// Returns a harness configuration whose provider is `server`.
    fn config(&self, server: &MockServer) -> NativeHarnessConfig {
        let environment = BTreeMap::from([
            (
                "AGENTTY_ROOT".to_string(),
                self.agentty_root.path().display().to_string(),
            ),
            (
                "HOME".to_string(),
                std::env::var("HOME").unwrap_or_default(),
            ),
            ("MODEL_API_BASE_URL".to_string(), server.uri()),
            ("MODEL_API_KEY".to_string(), "muse-key".to_string()),
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
        ]);

        NativeHarnessConfig::with_environment(self.data_root.path().to_path_buf(), move || {
            environment.clone()
        })
    }

    /// Asserts that the commit landed on the session branch, the base branch
    /// and packed references stayed unchanged, the temporary file is in the
    /// session's temporary directory, and nothing was written outside.
    fn assert_effects(&self) -> TestResult {
        let subject = |branch: &str| -> TestResult<String> {
            let log = Command::new("git")
                .args(["log", "-1", "--format=%s", branch])
                .current_dir(self.repository.path())
                .output()?;

            Ok(String::from_utf8_lossy(&log.stdout).trim().to_string())
        };
        assert_eq!(subject(SESSION_BRANCH)?, "Add notes");
        assert_eq!(subject("main")?, "Base");
        assert!(!self.repository.path().join(".git/packed-refs").exists());
        assert!(
            self.data_root
                .path()
                .join(SESSION_ID)
                .join("tmp")
                .join("scratch")
                .exists()
        );
        assert!(!self.escape().exists());

        Ok(())
    }

    /// Asserts that the repository the command built in its temporary
    /// directory and moved into the worktree is there, and that Agentty's
    /// host Git neither stages it nor runs its `core.fsmonitor` command.
    async fn assert_planted_repository_ignored(&self) -> TestResult {
        assert!(
            self.worktree.join("planted/.git").is_dir(),
            "the command should move its repository into the worktree"
        );
        let client = RealGitClient;
        client.stage_all(self.worktree.clone()).await?;
        client.worktree_status(self.worktree.clone()).await?;
        client
            .diff(self.worktree.clone(), "main".to_string())
            .await?;
        client
            .diff_stats(self.worktree.clone(), "main".to_string())
            .await?;
        let index = Command::new("git")
            .args(["ls-files", "--format=%(objectmode) %(path)"])
            .current_dir(&self.worktree)
            .output()?;
        let index = String::from_utf8_lossy(&index.stdout);
        assert!(!index.contains("160000"), "{index}");
        assert!(
            !self.marker().exists(),
            "the planted fsmonitor ran on the host"
        );

        Ok(())
    }

    /// Returns the file the command must not create outside the worktree.
    fn escape(&self) -> PathBuf {
        self.outside.path().join("escape")
    }

    /// Returns the command the planted repository names as its
    /// `core.fsmonitor`, which records that it ran.
    fn fsmonitor(&self) -> PathBuf {
        self.outside.path().join("fsmonitor.sh")
    }

    /// Returns the file the planted repository's `core.fsmonitor` creates.
    fn marker(&self) -> PathBuf {
        self.outside.path().join("fsmonitor-ran")
    }
}

/// Runs `git` in `directory` with a fixed identity.
fn git(directory: &Path, arguments: &[&str]) -> TestResult {
    let status = Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(arguments)
        .current_dir(directory)
        .status()?;
    if !status.success() {
        return Err(format!("git {arguments:?} failed with {status}").into());
    }

    Ok(())
}

/// Builds an edit-mode session turn for `folder`.
fn turn_request(folder: &Path, prompt: &str) -> TurnRequest {
    TurnRequest {
        context_compaction: ContextCompaction::Off,
        provider_call_budget: None,
        continuation: TurnContinuation::fresh(),
        execution_policy: ExecutionPolicy::default(),
        folder: folder.to_path_buf(),
        main_checkout_root: None,
        model: MODEL.to_string(),
        permission_mode: PermissionMode::AutoEdit,
        agent_profile: AgentDefinitionPrompt::default(),
        prompt: TurnPrompt::from_text(prompt.to_string()),
        reasoning_level: ReasoningLevel::Low,
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::Concise,
        session_branch: Some(SESSION_BRANCH.to_string()),
        speed_mode: SpeedMode::Normal,
    }
}

/// Returns a terminal structured answer.
fn answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"answer": text, "questions": []}).to_string()}
        }]
    }))
}

/// Returns one `bash` tool call running `command`.
fn bash_call(command: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call-bash",
                    "type": "function",
                    "function": {
                        "name": "bash",
                        "arguments": json!({"command": command}).to_string()
                    }
                }]
            }
        }]
    }))
}

/// Mounts one response for requests whose body contains `marker`.
async fn respond(server: &MockServer, marker: &str, response: ResponseTemplate, priority: u8) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(marker))
        .respond_with(response)
        .with_priority(priority)
        .mount(server)
        .await;
}
