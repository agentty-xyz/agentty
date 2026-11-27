//! Agentty's command grants under the real native sandbox: Bubblewrap on
//! Linux, Seatbelt on macOS. Missing isolation fails the test.
//!
//! The binary is its own test harness so that `main` can call
//! `dispatch_sandbox_helper` before anything else, as Agentty's `main`
//! does; the harness then starts this executable as the sandbox helper. It
//! answers the `cargo nextest` listing protocol, runs the tests a filter
//! selects, and fails on any other argument or a filter that selects
//! nothing, so it never passes without running.

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{self, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

use ag_agent::{
    NativeHarnessConfig, TrustedCargoCache, create_agent_channel, dispatch_sandbox_helper,
};
use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, ContextCompaction, ExecutionPolicy, HostCacheSharing,
    PermissionMode, ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnEvent,
    TurnNotice, TurnRequest,
};
use ag_harness::bash::Enforcement;
use ag_harness::tool::ToolDefinition;
use ag_protocol::TurnPrompt;
use ag_session::AgentKind;
use serde_json::json;
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::sync::mpsc;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Result of one fallible test step.
type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The tests this harness runs.
const TEST_NAMES: [&str; 2] = [
    "session_commands_write_a_linked_worktree_but_not_its_git_metadata",
    "session_gets_verified_copies_and_cannot_reach_the_cache",
];
/// Harness model served by the scripted provider.
const MODEL: &str = "muse-spark-1.3";
/// Session whose harness history the command tries to read.
const SESSION_ID: &str = "session-sandbox";
/// Session branch checked out in the worktree, nested like Agentty's own.
const SESSION_BRANCH: &str = "wt/session";
/// Stored API key file, under the Agentty root, the command tries to read.
const API_KEY_FILE: &str = "auth/api_keys.json";
/// Archive the host `cargo` stores for the locked crate.
const CACHED_ARCHIVE: &[u8] = b"leaf archive";

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
                TEST_NAMES.map(|name| format!("{name}: test\n")).concat()
            };

            io::stdout()
                .write_all(listing.as_bytes())
                .map_err(Box::<dyn Error>::from)
        }
        Ok(Invocation::Run(selected)) => run(&selected),
        Err(error) => Err(error.into()),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "native_sandbox: {error}");

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
    /// Run the selected tests.
    Run(Vec<&'static str>),
}

impl Invocation {
    /// Parses the runner's arguments: `--list --format terse [--ignored]`,
    /// or an optional name filter with `--exact` and `--nocapture`, as
    /// `cargo nextest` and plain `cargo test` pass them.
    ///
    /// # Errors
    /// Returns an error for any other argument, a second filter, or a
    /// filter that selects no test.
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
        let selected = TEST_NAMES
            .into_iter()
            .filter(|name| {
                filter.is_none_or(|filter| {
                    if exact {
                        filter == *name
                    } else {
                        name.contains(filter)
                    }
                })
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Err(format!(
                "the filter selects no test; the tests are {TEST_NAMES:?}"
            ));
        }

        Ok(Self::Run(selected))
    }
}

/// Runs the `selected` tests in order, starting their runtime only after
/// `main` dispatched.
#[tokio::main]
async fn run(selected: &[&str]) -> TestResult {
    for name in selected {
        let outcome = match *name {
            "session_gets_verified_copies_and_cannot_reach_the_cache" => {
                session_gets_verified_copies_and_cannot_reach_the_cache().await
            }
            _ => session_commands_write_a_linked_worktree_but_not_its_git_metadata().await,
        };
        outcome.map_err(|error| format!("{name}: {error}"))?;
    }

    Ok(())
}

/// Runs a scripted session turn whose one command writes a file in a linked
/// worktree, uses its temporary directory, and tries to commit, to write
/// outside the worktree, and to read the harness history.
async fn session_commands_write_a_linked_worktree_but_not_its_git_metadata() -> TestResult {
    // Arrange
    let fixture = Fixture::new()?;
    let server = MockServer::start().await;
    respond_when(
        &server,
        |body| body.contains(r#""tool_call_id":"call-bash""#) && !body.contains("Stop sharing"),
        answer("Done"),
        1,
    )
    .await;
    respond(
        &server,
        "Commit notes",
        bash_call(&fixture.command(), "call-bash"),
        2,
    )
    .await;
    let config = fixture.config(&server, BTreeMap::new());
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn(
            SESSION_ID.to_string(),
            turn_request(&fixture.worktree, None, "Commit notes"),
            events_tx,
        )
        .await;

    // Assert
    let notices = drain_notices(&mut events_rx);
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
    // The command writes a file and reads the repository with Git, then
    // staging, committing, moving the base branch, editing `packed-refs`,
    // the write outside, and the reads of the harness history and the
    // application database fail, so only the first marker reaches standard
    // output.
    assert!(
        bash_result.contains(r#"\"stdout\":\"written\\n\""#),
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

    host_caches_are_shared_only_when_the_turn_asks(&fixture, &server, &config).await
}

/// Runs a turn of another session sharing the host's caches, whose command
/// writes the Cargo registry and takes Cargo's cache lock through its
/// `CARGO_HOME` and tries to write the host Cargo home's programs,
/// configuration, and credentials.
async fn host_caches_are_shared_only_when_the_turn_asks(
    fixture: &Fixture,
    server: &MockServer,
    config: &NativeHarnessConfig,
) -> TestResult {
    // Arrange
    respond(
        server,
        "Share caches",
        bash_call(&fixture.cache_command(), "call-bash"),
        2,
    )
    .await;
    let channel = create_agent_channel(AgentKind::Harness, None, Some(config));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let request = TurnRequest {
        host_caches: HostCacheSharing::Shared,
        ..turn_request(&fixture.worktree, None, "Share caches")
    };

    // Act
    let result = channel
        .run_turn("session-shared".to_string(), request, events_tx)
        .await;

    // Assert
    assert_eq!(result?.assistant_message.answer, "Done");
    let mut notices = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        if let TurnEvent::Notice(notice) = event {
            notices.push(notice);
        }
    }
    assert!(
        notices.contains(&TurnNotice::HostCachesNotShared {
            caches: vec!["npm".to_string()],
        }),
        "the linked npm cache must be named: {notices:?}"
    );
    let bodies = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect::<Vec<_>>();
    let bash_result = bodies
        .iter()
        .rfind(|body| body.contains(r#""tool_call_id":"call-bash""#))
        .ok_or("bash result should reach the model")?;
    assert!(
        bash_result.contains(r#"\"stdout\":\"registry\\nlocked\\n\""#),
        "{bash_result}"
    );
    let cargo = fixture.home.path().join(".cargo");
    assert_eq!(
        std::fs::read_to_string(cargo.join("registry/shared"))?,
        "shared"
    );
    assert_eq!(
        std::fs::read_to_string(cargo.join(".package-cache"))?,
        "locked"
    );
    assert!(!cargo.join("bin/planted").exists());
    assert_eq!(std::fs::read_to_string(cargo.join("config.toml"))?, "");
    assert_eq!(std::fs::read_to_string(cargo.join("credentials.toml"))?, "");

    host_caches_stop_being_shared(fixture, server, config).await
}

/// Runs a turn of the same session after sharing was turned off, whose
/// command writes the registry through its `CARGO_HOME` and the host's
/// registry directly; only the session's own registry receives the write.
async fn host_caches_stop_being_shared(
    fixture: &Fixture,
    server: &MockServer,
    config: &NativeHarnessConfig,
) -> TestResult {
    // Arrange
    let cargo = fixture.home.path().join(".cargo");
    respond_when(
        server,
        |body| body.contains("Stop sharing") && !body.contains("call-off"),
        bash_call(
            &format!(
                "mkdir -p \"$CARGO_HOME/registry\" && printf session > \
                 \"$CARGO_HOME/registry/after\" && echo own; printf x > '{}/registry/direct' && \
                 echo host",
                cargo.display()
            ),
            "call-off",
        ),
        1,
    )
    .await;
    respond(server, r#""tool_call_id":"call-off""#, answer("Stopped"), 1).await;
    let channel = create_agent_channel(AgentKind::Harness, None, Some(config));
    let (events_tx, _events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn(
            "session-shared".to_string(),
            turn_request(&fixture.worktree, None, "Stop sharing"),
            events_tx,
        )
        .await;

    // Assert
    assert_eq!(result?.assistant_message.answer, "Stopped");
    let session_cargo = fixture.data_root.path().join("session-shared/tmp/cargo");
    assert!(!session_cargo.join("registry").is_symlink());
    assert_eq!(
        std::fs::read_to_string(session_cargo.join("registry/after"))?,
        "session"
    );
    assert!(!cargo.join("registry/after").exists());
    assert!(!cargo.join("registry/direct").exists());

    Ok(())
}

/// Fills the project's trusted Cargo cache through a host `cargo` that
/// stores a crates.io archive, then runs a session turn whose `CARGO_HOME`
/// holds a verified copy of it, and whose commands can neither read, list,
/// nor write the cache.
async fn session_gets_verified_copies_and_cannot_reach_the_cache() -> TestResult {
    // Arrange
    let fixture = Fixture::new()?;
    let (host, _temporary) = fixture.host_cargo()?;
    let cache_root = fixture.agentty_root.path().join("cache/cargo");
    let cache = TrustedCargoCache::for_project(&cache_root, fixture.repository.path());
    let fetched = AtomicBool::new(false);
    cache
        .refresh("cargo", &fixture.worktree, &host, || {
            fetched.store(true, Ordering::SeqCst);
        })
        .await?;
    let archive = fixture.cached_archive()?;
    let planted = archive
        .ancestors()
        .nth(4)
        .ok_or("cache home")?
        .join("planted");
    let command = format!(
        "cat \"$CARGO_HOME\"/registry/cache/index.crates.io-*/leaf-1.0.0.crate > /dev/null && \
         echo copied; printf x >> '{archive}' && echo wrote-cache; touch '{}' && echo \
         planted-cache; cat '{archive}' > /dev/null && echo read-cache; [ -z \"$(ls -A '{}' 2> \
         /dev/null)\" ] && echo cache-hidden",
        planted.display(),
        cache_root.display(),
        archive = archive.display(),
    );
    let server = MockServer::start().await;
    respond(&server, r#""tool_call_id":"call-bash""#, answer("Done"), 1).await;
    respond(
        &server,
        "Use the cache",
        bash_call(&command, "call-bash"),
        2,
    )
    .await;
    let config = fixture
        .config(&server, BTreeMap::new())
        .with_trusted_cargo_cache(cache_root);
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let result = channel
        .run_turn(
            SESSION_ID.to_string(),
            turn_request(
                &fixture.worktree,
                Some(fixture.repository.path()),
                "Use the cache",
            ),
            events_tx,
        )
        .await;

    // Assert
    assert!(fetched.load(Ordering::SeqCst));
    let notices = drain_notices(&mut events_rx);
    assert!(
        notices.contains(&TurnNotice::SandboxAvailable),
        "the native sandbox must run here: {notices:?}"
    );
    assert_eq!(result?.assistant_message.answer, "Done");
    let bash_result = bash_result(&server, "Use the cache").await?;
    assert!(
        // macOS denies listing the hidden cache; Linux shows it empty.
        bash_result.contains(r#"\"stdout\":\"copied\\ncache-hidden\\n\""#),
        "{bash_result}"
    );
    let copy = fixture.session_copy(&archive)?;
    assert!(std::fs::symlink_metadata(&copy)?.is_file());
    assert_eq!(std::fs::read(&copy)?, CACHED_ARCHIVE);
    assert_eq!(std::fs::read(&archive)?, CACHED_ARCHIVE);
    assert!(!planted.exists());

    Ok(())
}

/// Returns the notices among the turn events `events` received so far.
fn drain_notices(events: &mut mpsc::UnboundedReceiver<TurnEvent>) -> Vec<TurnNotice> {
    let mut notices = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let TurnEvent::Notice(notice) = event {
            notices.push(notice);
        }
    }

    notices
}

/// Returns the request that carried the `bash` result of the turn started
/// with `prompt` to `server`.
async fn bash_result(server: &MockServer, prompt: &str) -> TestResult<String> {
    let bodies = server.received_requests().await.unwrap_or_default();

    bodies
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .find(|body| body.contains(r#""tool_call_id":"call-bash""#) && body.contains(prompt))
        .ok_or_else(|| "bash result should reach the model".into())
}

/// A repository with a linked session worktree, a directory outside it, the
/// harness data root, and an Agentty root holding the application database.
struct Fixture {
    agentty_root: TempDir,
    data_root: TempDir,
    /// The host's home directory, with a Cargo home holding programs,
    /// configuration, and credentials.
    home: TempDir,
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

        let home = tempfile::tempdir()?;
        std::fs::create_dir_all(home.path().join(".cargo/bin"))?;
        for file in ["config.toml", "credentials.toml"] {
            std::fs::write(home.path().join(".cargo").join(file), "")?;
        }
        // An npm cache moved elsewhere through a link stays per session.
        std::fs::create_dir(home.path().join("npm-elsewhere"))?;
        std::os::unix::fs::symlink(home.path().join("npm-elsewhere"), home.path().join(".npm"))?;

        Ok(Self {
            agentty_root,
            data_root: tempfile::tempdir()?,
            home,
            outside: tempfile::tempdir()?,
            repository,
            worktree,
            _worktrees: worktrees,
        })
    }

    /// Returns the command the model runs: a worktree file, a temporary
    /// file, and Git reads, then staging, a commit, moves of the base branch
    /// and edits of `packed-refs`, a write outside the worktree, and reads of
    /// the harness history and of Agentty's application database and stored
    /// API keys.
    fn command(&self) -> String {
        format!(
            "export GIT_AUTHOR_NAME=Agentty GIT_AUTHOR_EMAIL=agentty@example.com \
             GIT_COMMITTER_NAME=Agentty GIT_COMMITTER_EMAIL=agentty@example.com; printf \
             'sandboxed\\n' > notes.txt && touch \"$TMPDIR/scratch\" && git status --porcelain | \
             grep -q notes.txt && git log -1 > /dev/null && echo written; git add notes.txt \
             2>/dev/null && echo staged; git commit -qam 'Add notes' 2>/dev/null && echo \
             committed; git update-ref refs/heads/main HEAD 2>/dev/null && echo moved-main; \
             printf x >> \"$(git rev-parse --path-format=absolute --git-common-dir)/packed-refs\" \
             && echo edited-packed-refs; touch '{}' && echo escaped; cat '{}' > /dev/null && echo \
             read-history; cat '{}' > /dev/null && echo read-database; cat '{}' > /dev/null && \
             echo read-keys",
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

    /// Returns the command a session sharing the host's caches runs: it
    /// writes the registry and takes the cache lock through `CARGO_HOME`,
    /// then tries to write the host Cargo home's programs, configuration,
    /// and credentials, so only the first two markers reach standard
    /// output.
    fn cache_command(&self) -> String {
        format!(
            "printf shared > \"$CARGO_HOME/registry/shared\" && echo registry; perl -e 'open(my \
             $f, \"+<\", $ARGV[0]) or exit 2; flock($f, 2) or exit 3; print $f \"locked\" or exit \
             4; close($f) or exit 5' \"$CARGO_HOME/.package-cache\" && echo locked; printf x > \
             '{cargo}/bin/planted' && echo bin; printf x >> '{cargo}/config.toml' && echo config; \
             printf x >> '{cargo}/credentials.toml' && echo credentials",
            cargo = self.home.path().join(".cargo").display(),
        )
    }

    /// Returns a harness configuration whose provider is `server`, with the
    /// `extra` host variables.
    fn config(&self, server: &MockServer, extra: BTreeMap<String, String>) -> NativeHarnessConfig {
        let mut environment = BTreeMap::from([
            (
                "AGENTTY_ROOT".to_string(),
                self.agentty_root.path().display().to_string(),
            ),
            ("HOME".to_string(), self.home.path().display().to_string()),
            ("MODEL_API_BASE_URL".to_string(), server.uri()),
            ("MODEL_API_KEY".to_string(), "muse-key".to_string()),
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
        ]);
        environment.extend(extra);

        NativeHarnessConfig::with_environment(self.data_root.path().to_path_buf(), move || {
            environment.clone()
        })
    }

    /// Asserts that the file landed in the worktree for Agentty to commit,
    /// while the index, the session and base branches, and packed
    /// references stayed unchanged, the temporary file is in the session's
    /// temporary directory, and nothing was written outside.
    fn assert_effects(&self) -> TestResult {
        let subject = |branch: &str| -> TestResult<String> {
            let log = Command::new("git")
                .args(["log", "-1", "--format=%s", branch])
                .current_dir(self.repository.path())
                .output()?;

            Ok(String::from_utf8_lossy(&log.stdout).trim().to_string())
        };
        assert_eq!(subject(SESSION_BRANCH)?, "Base");
        assert_eq!(subject("main")?, "Base");
        assert_eq!(
            std::fs::read_to_string(self.worktree.join("notes.txt"))?,
            "sandboxed\n"
        );
        let staged = Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&self.worktree)
            .output()?;
        assert!(staged.status.success() && staged.stdout.is_empty());
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

    /// Locks `leaf` from crates.io in the worktree and returns a host
    /// environment whose `cargo` stores [`CACHED_ARCHIVE`] as that crate's
    /// archive in a canonical crates.io registry, as a fetch would, and
    /// whose `TMPDIR` is a private directory, kept until the guard drops.
    fn host_cargo(&self) -> TestResult<(BTreeMap<String, String>, TempDir)> {
        std::fs::write(
            self.worktree.join("Cargo.lock"),
            format!(
                "version = 4\n\n[[package]]\nname = \"leaf\"\nversion = \"1.0.0\"\nsource = \
                 \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n",
                hex::encode(Sha256::digest(CACHED_ARCHIVE))
            ),
        )?;
        let bin = self.outside.path().join("bin");
        std::fs::create_dir_all(&bin)?;
        let registry = "index.crates.io-0123456789abcdef";
        std::fs::write(
            bin.join("cargo"),
            format!(
                "#!/bin/sh\n[ \"$1\" = -V ] && {{ echo 'cargo 1.99.0'; exit 0; }}\nmkdir -p \
                 \"$CARGO_HOME/registry/cache/{registry}\" \
                 \"$CARGO_HOME/registry/index/{registry}/.cache/le/af\"\nprintf '%s' '{}' > \
                 \"$CARGO_HOME/registry/cache/{registry}/leaf-1.0.0.crate\"\nprintf '%s' \
                 '{{\"dl\":\"https://static.crates.io/crates\"}}' > \
                 \"$CARGO_HOME/registry/index/{registry}/config.json\"\nprintf entry > \
                 \"$CARGO_HOME/registry/index/{registry}/.cache/le/af/leaf\"\n",
                String::from_utf8_lossy(CACHED_ARCHIVE),
            ),
        )?;
        std::fs::set_permissions(bin.join("cargo"), std::fs::Permissions::from_mode(0o755))?;
        let temporary = private_temporary_directory()?;
        let environment = BTreeMap::from([
            (
                "PATH".to_string(),
                format!("{}:/usr/bin:/bin", bin.display()),
            ),
            ("TMPDIR".to_string(), temporary.path().display().to_string()),
        ]);

        Ok((environment, temporary))
    }

    /// Returns where the session's `CARGO_HOME` holds its copy of the
    /// cached `archive`.
    fn session_copy(&self, archive: &Path) -> TestResult<PathBuf> {
        let registry = archive
            .parent()
            .and_then(Path::file_name)
            .ok_or("registry")?;

        Ok(self
            .data_root
            .path()
            .join(SESSION_ID)
            .join("tmp/cargo/registry/cache")
            .join(registry)
            .join("leaf-1.0.0.crate"))
    }

    /// Returns the archive of `leaf` in the project's trusted Cargo cache.
    fn cached_archive(&self) -> TestResult<PathBuf> {
        let projects = self.agentty_root.path().join("cache/cargo");
        let project = std::fs::read_dir(projects)?
            .next()
            .ok_or("one project cache")??
            .path();
        let registry = std::fs::read_dir(project.join("home/registry/cache"))?
            .next()
            .ok_or("one registry")??
            .path();

        Ok(registry.join("leaf-1.0.0.crate"))
    }

    /// Returns the file the command must not create outside the worktree.
    fn escape(&self) -> PathBuf {
        self.outside.path().join("escape")
    }
}

/// Returns a fresh directory where the trusted Cargo fetch may run: under
/// the first of the test process's `TMPDIR`, `XDG_RUNTIME_DIR`, CI runner
/// temporary directory, and home directory whose every ancestor only root
/// or the current user can change and has no Cargo configuration or
/// toolchain file, since a shared `/tmp` does not qualify.
fn private_temporary_directory() -> TestResult<TempDir> {
    let user = rustix::process::geteuid().as_raw();
    let is_private = |parent: &Path| {
        std::fs::canonicalize(parent).is_ok_and(|parent| {
            parent.ancestors().all(|ancestor| {
                let owned = std::fs::metadata(ancestor).is_ok_and(|metadata| {
                    (metadata.uid() == 0 || metadata.uid() == user) && metadata.mode() & 0o022 == 0
                });
                let configured = [
                    ".cargo/config.toml",
                    ".cargo/config",
                    "rust-toolchain.toml",
                    "rust-toolchain",
                ]
                .iter()
                .any(|name| ancestor.join(name).exists());

                owned && !configured
            })
        })
    };
    let parent = ["TMPDIR", "XDG_RUNTIME_DIR", "RUNNER_TEMP", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|parent| is_private(parent))
        .ok_or("no private temporary directory")?;

    Ok(tempfile::tempdir_in(parent)?)
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

/// Builds an edit-mode session turn for `folder`, a worktree of the
/// project whose main checkout is `main_checkout`.
fn turn_request(folder: &Path, main_checkout: Option<&Path>, prompt: &str) -> TurnRequest {
    TurnRequest {
        context_compaction: ContextCompaction::Off,
        provider_call_budget: None,
        continuation: TurnContinuation::fresh(),
        execution_policy: ExecutionPolicy::default(),
        folder: folder.to_path_buf(),
        main_checkout_root: main_checkout.map(Path::to_path_buf),
        model: MODEL.to_string(),
        permission_mode: PermissionMode::AutoEdit,
        agent_profile: AgentDefinitionPrompt::default(),
        prompt: TurnPrompt::from_text(prompt.to_string()),
        reasoning_level: ReasoningLevel::Low,
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::Concise,
        host_caches: HostCacheSharing::PerSession,
        speed_mode: SpeedMode::Normal,
        task_milestone: None,
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

/// Returns one `bash` tool call `id` running `command`.
fn bash_call(command: &str, id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": id,
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

/// Mounts one response for requests whose body `matches`.
async fn respond_when(
    server: &MockServer,
    matches: fn(&str) -> bool,
    response: ResponseTemplate,
    priority: u8,
) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(move |request: &wiremock::Request| matches(&String::from_utf8_lossy(&request.body)))
        .respond_with(response)
        .with_priority(priority)
        .mount(server)
        .await;
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
