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
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

use ag_agent::{
    NativeHarnessConfig, TrustedCargoCache, create_agent_channel, dispatch_sandbox_helper,
};
use ag_contracts::{
    AgentDefinitionPrompt, AgentRequestKind, ContextCompaction, ExecutionPolicy, PermissionMode,
    ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnEvent, TurnNotice, TurnRequest,
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
    "session_commands_commit_in_a_linked_worktree_and_stay_confined",
    "session_cargo_reuses_the_trusted_cache_without_the_registry",
];
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
            "session_cargo_reuses_the_trusted_cache_without_the_registry" => {
                session_cargo_reuses_the_trusted_cache_without_the_registry().await
            }
            _ => session_commands_commit_in_a_linked_worktree_and_stay_confined().await,
        };
        outcome.map_err(|error| format!("{name}: {error}"))?;
    }

    Ok(())
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
    fixture.assert_effects()
}

/// Fills the project's trusted Cargo cache from a local registry that the
/// host's `cargo` uses for crates.io, while the worktree holds Cargo
/// configuration and a toolchain file the fetch must ignore. A session of
/// the project then resolves its locked dependency online from its copies
/// without a single registry request, where the fetch into the empty cache
/// made several, and can neither read nor write the cache.
async fn session_cargo_reuses_the_trusted_cache_without_the_registry() -> TestResult {
    // Arrange
    let fixture = Fixture::new()?;
    let registry = LocalRegistry::start(&fixture).await?;
    let cache_root = fixture.agentty_root.path().join("cache/cargo");
    let cache = TrustedCargoCache::for_project(&cache_root, fixture.repository.path());
    let wrapper_ran = fixture.plant_cargo_configuration()?;
    let fetched = AtomicBool::new(false);
    let before_fetch = registry.request_count().await;
    cache
        .refresh("cargo", &fixture.worktree, &registry.environment, || {
            fetched.store(true, Ordering::SeqCst);
        })
        .await?;
    fixture.remove_cargo_configuration()?;
    let archive = fixture.cached_archive()?;
    let archive_bytes = std::fs::read(&archive)?;
    let project = archive.ancestors().nth(5).ok_or("project cache")?;
    let planted = project.join("home/planted");
    let resolve = "cargo metadata --locked --format-version 1 > /dev/null && echo resolved";
    let cached_command = format!(
        "{resolve}; printf x >> '{archive}' && echo wrote-cache; touch '{}' && echo \
         planted-cache; cat '{archive}' > /dev/null && echo read-cache; cat '{}' > /dev/null && \
         echo read-workspace",
        planted.display(),
        project.join("private/work/Cargo.toml").display(),
        archive = archive.display(),
    );
    let server = MockServer::start().await;
    respond(&server, r#""tool_call_id":"call-bash""#, answer("Done"), 1).await;
    respond(&server, "Build cached", bash_call(&cached_command), 2).await;
    let config = fixture
        .config(&server, registry.environment.clone())
        .with_trusted_cargo_cache(cache_root);
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let before = registry.request_count().await;

    // Act
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let cached = channel
        .run_turn(
            SESSION_ID.to_string(),
            turn_request(
                &fixture.worktree,
                Some(fixture.repository.path()),
                "Build cached",
            ),
            events_tx,
        )
        .await;
    let after_cached = registry.request_count().await;

    // Assert
    assert!(fetched.load(Ordering::SeqCst));
    assert!(
        !wrapper_ran.exists(),
        "the worktree's rustc wrapper must not run"
    );
    let notices = drain_notices(&mut events_rx);
    assert!(
        notices.contains(&TurnNotice::SandboxAvailable),
        "the native sandbox must run here: {notices:?}"
    );
    assert_eq!(cached?.assistant_message.answer, "Done");
    let bash_result = bash_result(&server, "Build cached").await?;
    assert!(
        bash_result.contains(r#"\"stdout\":\"resolved\\n\""#),
        "{bash_result}"
    );
    assert!(
        before > before_fetch,
        "the empty cache's fetch downloads from the registry"
    );
    assert_eq!(
        after_cached, before,
        "the session with the cache must not reach the registry"
    );
    let copy = fixture.session_copy(&archive)?;
    assert!(std::fs::symlink_metadata(&copy)?.is_file());
    assert_eq!(std::fs::read(&copy)?, archive_bytes);
    assert_eq!(std::fs::read(&archive)?, archive_bytes);
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

/// A sparse Cargo registry served from this process in place of crates.io,
/// holding one crate, `leaf`, which the fixture worktree depends on and has
/// locked.
struct LocalRegistry {
    /// Host environment whose `cargo` on the `PATH` replaces crates.io with
    /// the registry.
    environment: BTreeMap<String, String>,
    server: MockServer,
    _host: TempDir,
}

impl LocalRegistry {
    /// Packages `leaf` with Cargo, serves it, puts a `cargo` that replaces
    /// crates.io with it first on the host `PATH`, and locks the worktree's
    /// dependency on it.
    async fn start(fixture: &Fixture) -> TestResult<Self> {
        let host = tempfile::tempdir()?;
        let mut environment = ["HOME", "PATH", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"]
            .into_iter()
            .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
            .collect::<BTreeMap<_, _>>();
        let cargo_home = host.path().join("cargo");
        environment.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
        let leaf = host.path().join("leaf");
        std::fs::create_dir_all(leaf.join("src"))?;
        std::fs::write(
            leaf.join("Cargo.toml"),
            "[package]\nname = \"leaf\"\nversion = \"1.0.0\"\nedition = \"2021\"\nlicense = \
             \"MIT\"\ndescription = \"Test crate\"\n",
        )?;
        std::fs::write(leaf.join("src/lib.rs"), "pub fn leaf() {}\n")?;
        let target = host.path().join("target");
        cargo(
            &leaf,
            &environment,
            &target,
            &["package", "--no-verify", "--allow-dirty", "--offline"],
        )?;
        let archive = std::fs::read(target.join("package/leaf-1.0.0.crate"))?;
        let checksum = hex::encode(Sha256::digest(&archive));

        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("GET"))
            .and(path("/index/config.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"dl": format!("{uri}/dl/{{crate}}/{{version}}")})),
            )
            .mount(&server)
            .await;
        let entry = json!({
            "name": "leaf", "vers": "1.0.0", "deps": [], "cksum": checksum,
            "features": {}, "yanked": false
        });
        Mock::given(method("GET"))
            .and(path("/index/le/af/leaf"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("{entry}\n")))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/dl/leaf/1.0.0"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .mount(&server)
            .await;
        std::fs::create_dir_all(&cargo_home)?;
        // The trusted cache skips Cargo configuration that replaces
        // crates.io, so the host's `cargo` itself points crates.io here.
        let real_cargo = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join("cargo"))
            .find(|path| path.is_file())
            .ok_or("cargo not found on PATH")?;
        let wrapper = host.path().join("bin");
        std::fs::create_dir_all(&wrapper)?;
        std::fs::write(
            wrapper.join("cargo"),
            format!(
                "#!/bin/sh\nexec '{}' --config 'source.crates-io.replace-with=\"local\"' --config \
                 'source.local.registry=\"sparse+{uri}/index/\"' \"$@\"\n",
                real_cargo.display()
            ),
        )?;
        std::fs::set_permissions(
            wrapper.join("cargo"),
            std::fs::Permissions::from_mode(0o755),
        )?;
        let path = std::env::join_paths(std::iter::once(wrapper).chain(std::env::split_paths(
            environment.get("PATH").map_or("", String::as_str),
        )))?;
        environment.insert("PATH".to_string(), path.to_string_lossy().into_owned());

        std::fs::write(
            fixture.worktree.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \
             \"2021\"\n\n[dependencies]\nleaf = \"1\"\n",
        )?;
        std::fs::create_dir_all(fixture.worktree.join("src"))?;
        std::fs::write(fixture.worktree.join("src/main.rs"), "fn main() {}\n")?;
        cargo(
            &fixture.worktree,
            &environment,
            &target,
            &["generate-lockfile"],
        )?;

        Ok(Self {
            environment,
            server,
            _host: host,
        })
    }

    /// Returns how many requests the registry served.
    async fn request_count(&self) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .len()
    }
}

/// Runs Cargo with `arguments` in `directory`, with only `environment` and
/// `target` as its target directory.
fn cargo(
    directory: &Path,
    environment: &BTreeMap<String, String>,
    target: &Path,
    arguments: &[&str],
) -> TestResult {
    let status = Command::new("cargo")
        .args(arguments)
        .current_dir(directory)
        .env_clear()
        .envs(environment)
        .env("CARGO_TARGET_DIR", target)
        .status()?;
    if !status.success() {
        return Err(format!("cargo {arguments:?} failed with {status}").into());
    }

    Ok(())
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

        Ok(Self {
            agentty_root,
            data_root: tempfile::tempdir()?,
            outside: tempfile::tempdir()?,
            repository,
            worktree,
            _worktrees: worktrees,
        })
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
             touch \"$TMPDIR/scratch\" && echo committed; git update-ref refs/heads/main HEAD \
             2>/dev/null && echo moved-main; printf x >> \"$(git rev-parse --path-format=absolute \
             --git-common-dir)/packed-refs\" && echo edited-packed-refs; touch '{}' && echo \
             escaped; cat '{}' > /dev/null && echo read-history; cat '{}' > /dev/null && echo \
             read-database; cat '{}' > /dev/null && echo read-keys",
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

    /// Returns a harness configuration whose provider is `server`, with the
    /// `extra` host variables.
    fn config(&self, server: &MockServer, extra: BTreeMap<String, String>) -> NativeHarnessConfig {
        let mut environment = BTreeMap::from([
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
        environment.extend(extra);

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

    /// Plants Cargo configuration naming a `build.rustc-wrapper` and a
    /// `rust-toolchain.toml` naming a missing `path` toolchain in the
    /// worktree, as a session could, and returns the file the wrapper
    /// creates when it runs.
    fn plant_cargo_configuration(&self) -> TestResult<PathBuf> {
        let marker = self.outside.path().join("wrapper-ran");
        let wrapper = self.outside.path().join("wrapper");
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\ntouch '{}'\nexec \"$@\"\n", marker.display()),
        )?;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))?;
        std::fs::create_dir_all(self.worktree.join(".cargo"))?;
        std::fs::write(
            self.worktree.join(".cargo/config.toml"),
            format!("[build]\nrustc-wrapper = '{}'\n", wrapper.display()),
        )?;
        std::fs::write(
            self.worktree.join("rust-toolchain.toml"),
            "[toolchain]\npath = \"/nonexistent-toolchain\"\n",
        )?;

        Ok(marker)
    }

    /// Removes what [`Self::plant_cargo_configuration`] planted, so that the
    /// session's own Cargo runs normally.
    fn remove_cargo_configuration(&self) -> TestResult {
        std::fs::remove_dir_all(self.worktree.join(".cargo"))?;
        std::fs::remove_file(self.worktree.join("rust-toolchain.toml"))?;

        Ok(())
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
