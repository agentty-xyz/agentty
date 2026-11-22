//! Agentty's command grants under the real native sandbox: Bubblewrap on
//! Linux, Seatbelt on macOS. Missing isolation fails the test.
//!
//! The binary is its own test harness so that `main` can call
//! `dispatch_sandbox_helper` before anything else, as Agentty's `main`
//! does; the harness then starts this executable as the sandbox helper. It
//! answers the `cargo nextest` listing protocol and runs its one test
//! otherwise.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ag_agent::{NativeHarnessConfig, create_agent_channel, dispatch_sandbox_helper};
use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, ContextCompaction, ExecutionPolicy, PermissionMode,
    ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnEvent, TurnNotice, TurnRequest,
};
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
/// Session whose harness directory holds the command's temporary directory.
const SESSION_ID: &str = "session-sandbox";

fn main() -> ExitCode {
    if let Some(helper) = dispatch_sandbox_helper() {
        return helper;
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let has = |flag: &str| arguments.iter().any(|argument| argument == flag);
    if has("--list") {
        // This harness has no ignored tests.
        let listing = if has("--ignored") {
            String::new()
        } else {
            format!("{TEST_NAME}: test\n")
        };

        return match io::stdout().write_all(listing.as_bytes()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    let selected = arguments
        .iter()
        .find(|argument| !argument.starts_with("--"))
        .is_none_or(|filter| {
            if has("--exact") {
                filter == TEST_NAME
            } else {
                TEST_NAME.contains(filter.as_str())
            }
        });
    if !selected {
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "{TEST_NAME} failed: {error}");

            ExitCode::FAILURE
        }
    }
}

/// Runs the test, starting its runtime only after `main` dispatched.
#[tokio::main]
async fn run() -> TestResult {
    session_commands_commit_in_a_linked_worktree_and_stay_confined().await
}

/// Runs a scripted session turn whose one command commits in a linked
/// worktree, uses its temporary directory, and tries to write outside the
/// worktree and to read the harness history.
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
    // The command commits, then the write outside and the history read
    // fail, so only the first marker reaches standard output.
    assert!(
        bash_result.contains(r#"\"stdout\":\"committed\\n\""#),
        "{bash_result}"
    );
    assert!(
        notices.contains(&TurnNotice::LikelySandboxDenial),
        "the denied write must be reported: {notices:?}"
    );
    fixture.assert_effects()
}

/// A repository with a linked session worktree, a directory outside it, and
/// the harness data root.
struct Fixture {
    data_root: TempDir,
    outside: TempDir,
    repository: TempDir,
    worktree: PathBuf,
    _worktrees: TempDir,
}

impl Fixture {
    /// Creates the repository with one commit and its linked worktree on the
    /// `session` branch.
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
            &["worktree", "add", "-q", "-b", "session", &worktree_path],
        )?;

        Ok(Self {
            data_root: tempfile::tempdir()?,
            outside: tempfile::tempdir()?,
            repository,
            worktree,
            _worktrees: worktrees,
        })
    }

    /// Returns the command the model runs: a commit, a temporary file, a
    /// write outside the worktree, and a read of the harness history.
    fn command(&self) -> String {
        format!(
            "printf 'sandboxed\\n' > notes.txt && git add notes.txt && git -c user.name=Agentty \
             -c user.email=agentty@example.com commit -qm 'Add notes' && touch \
             \"$TMPDIR/scratch\" && echo committed; touch '{}' && echo escaped; cat '{}' > \
             /dev/null && echo read-history",
            self.escape().display(),
            self.data_root
                .path()
                .join(SESSION_ID)
                .join("harness.db")
                .display(),
        )
    }

    /// Returns a harness configuration whose provider is `server`.
    fn config(&self, server: &MockServer) -> NativeHarnessConfig {
        let environment = BTreeMap::from([
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

    /// Asserts that the commit landed on the session branch, the temporary
    /// file exists, and nothing was written outside.
    fn assert_effects(&self) -> TestResult {
        let log = Command::new("git")
            .args(["log", "-1", "--format=%s", "session"])
            .current_dir(self.repository.path())
            .output()?;
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "Add notes");
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

    /// Returns the file the command must not create outside the worktree.
    fn escape(&self) -> PathBuf {
        self.outside.path().join("escape")
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
