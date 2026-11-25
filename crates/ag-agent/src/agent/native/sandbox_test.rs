//! Native sandbox grant and notice unit tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ag_contracts::{HostCacheSharing, TurnNotice};
use ag_harness::RunEventKind;
use ag_harness::bash::BashConfig;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use serde_json::json;

use super::{
    CargoCacheSource, CommandSandbox, SESSION_SANDBOX, ShareLimits, UTILITY_SANDBOX, notice,
};
use crate::agent::native::cargo_cache::TrustedCargoCache;

/// Returns a native policy without Agentty's grants.
fn base_policy() -> BashConfig {
    BashConfig::native(
        PathBuf::from("/bin/bash"),
        "test-revision".to_string(),
        Duration::from_secs(60),
        BashConfig::DEFAULT_CAPTURE_BYTES,
    )
    .expect("base policy")
}

/// Returns the policy Agentty's grants must produce for `temporary` and
/// the hidden `data_root` and `auth_file`, with the stored API keys,
/// application database and pasted images of the Agentty root
/// `/agentty-root` hidden too.
fn expected_policy(temporary: &Path, data_root: &Path, auth_file: &Path) -> BashConfig {
    expected_shared_policy(temporary, data_root, auth_file, &HostCaches::default())
}

/// The host's caches a sandbox shares, in grant order.
#[derive(Default)]
struct HostCaches {
    directories: Vec<PathBuf>,
    files: Vec<PathBuf>,
    variables: BTreeMap<&'static str, PathBuf>,
}

/// Returns [`expected_policy`] with `caches` shared.
fn expected_shared_policy(
    temporary: &Path,
    data_root: &Path,
    auth_file: &Path,
    caches: &HostCaches,
) -> BashConfig {
    let mut expected = base_policy()
        .with_full_disk_read()
        .and_then(|config| config.with_write(PathBuf::from(".")))
        .map(BashConfig::with_network)
        .and_then(|config| config.with_temporary_directory(temporary.to_path_buf()))
        .and_then(|config| config.with_hidden_path(data_root.to_path_buf()))
        .and_then(|config| config.with_hidden_path(auth_file.to_path_buf()))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/auth")))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/db")))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/tmp")))
        .expect("expected grants");
    for directory in &caches.directories {
        expected = expected
            .with_host_cache_directory(directory.clone())
            .expect("host cache");
    }
    for file in &caches.files {
        expected = expected
            .with_host_cache_file(file.clone())
            .expect("host cache file");
    }
    for (name, directory) in [
        ("CARGO_HOME", "cargo"),
        ("GOCACHE", "go-build"),
        ("GOMODCACHE", "go-mod"),
        ("PIP_CACHE_DIR", "pip"),
        ("XDG_CACHE_HOME", "cache"),
        ("npm_config_cache", "npm"),
    ] {
        let value = caches
            .variables
            .get(name)
            .cloned()
            .unwrap_or_else(|| temporary.join(directory))
            .display()
            .to_string();
        expected = expected
            .with_environment(name.to_string(), value)
            .expect("cache variable");
    }
    // Only Cargo shares lock files; its automatic cleaning stays off then.
    if !caches.files.is_empty() {
        expected = expected
            .with_environment(
                "CARGO_CACHE_AUTO_CLEAN_FREQUENCY".to_string(),
                "never".to_string(),
            )
            .expect("Cargo cleaning");
    }

    expected
}

/// Prepares `temporary` for commands in `workspace` with every cache per
/// session.
async fn per_session(
    data_root: &Path,
    temporary: PathBuf,
    workspace: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<CommandSandbox, String> {
    CommandSandbox::prepare(
        data_root,
        temporary,
        workspace,
        environment,
        None,
        HostCacheSharing::PerSession,
    )
    .await
}

#[test]
/// Session commands fall back to running without the sandbox, with a
/// warning; utility prompts, which report no notices, refuse instead.
fn sessions_fall_back_and_utility_prompts_refuse() {
    // Arrange, Act, Assert
    assert_eq!(
        SESSION_SANDBOX,
        Sandbox::Native {
            if_unavailable: IfUnavailable::Unsandboxed
        }
    );
    assert_eq!(
        UTILITY_SANDBOX,
        Sandbox::Native {
            if_unavailable: IfUnavailable::Refuse
        }
    );
}

#[test]
/// Grants full-disk reads, worktree and temporary-directory writes, the
/// network, and caches in the temporary directory, and hides the data root
/// and the `ChatGPT` sign-in, with no Git writes, writable roots, sockets
/// (not even an SSH agent's), or excluded commands.
fn grant_adds_the_coding_agent_grant_set() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let temporary = data_root.path().join("session-1").join("tmp");
    let socket_directory = tempfile::tempdir().expect("socket directory");
    let socket = socket_directory.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("socket");
    let environment = BTreeMap::from([
        ("AGENTTY_ROOT".to_string(), "/agentty-root".to_string()),
        ("SSH_AUTH_SOCK".to_string(), socket.display().to_string()),
    ]);
    let sandbox = CommandSandbox::new(data_root.path(), temporary.clone(), &environment);

    // Act
    let granted = sandbox.grant(base_policy());

    // Assert
    assert_eq!(
        granted.expect("granted policy"),
        expected_policy(
            &temporary,
            data_root.path(),
            Path::new("/agentty-root/auth/chatgpt.json"),
        )
    );
}

#[test]
/// Words every unavailability warning for Agentty's users, naming Agentty
/// in place of the harness's generic application.
fn unavailability_warnings_name_agentty_and_say_whether_commands_are_sandboxed() {
    // Arrange
    let reasons = [
        SandboxUnavailableReason::UnsupportedPlatform,
        SandboxUnavailableReason::HelperMissing,
        SandboxUnavailableReason::HelperUntrusted,
        SandboxUnavailableReason::BubblewrapMissing,
        SandboxUnavailableReason::UserNamespacesUnavailable,
        SandboxUnavailableReason::LandlockUnavailable,
        SandboxUnavailableReason::SeatbeltMissing,
        SandboxUnavailableReason::SeatbeltUnavailable,
        SandboxUnavailableReason::UnixSocketsUnsupported,
    ];

    // Act
    let notices = reasons.map(|reason| {
        notice(&RunEventKind::SandboxUnavailable(SandboxWarning::new(
            reason,
        )))
    });

    // Assert
    for (reason, notice) in reasons.into_iter().zip(notices) {
        let (message, remedy) = match notice {
            Some(TurnNotice::SandboxUnavailable { message, remedy }) => (message, remedy),
            other => (format!("{reason:?} gave {other:?}"), String::new()),
        };
        assert!(!remedy.contains("the application"), "{remedy}");
        assert!(message.ends_with(reason.message()), "{message}");
        assert_eq!(
            message.starts_with("Commands run without the sandbox. "),
            reason != SandboxUnavailableReason::UnixSocketsUnsupported,
            "{message}"
        );
    }
    assert_eq!(
        notice(&RunEventKind::SandboxUnavailable(SandboxWarning::new(
            SandboxUnavailableReason::BubblewrapMissing,
        ))),
        Some(TurnNotice::SandboxUnavailable {
            message: "Commands run without the sandbox. Bubblewrap (`bwrap`) is not installed."
                .to_string(),
            remedy: "Install bubblewrap, for example with `sudo apt install bubblewrap` or `sudo \
                     dnf install bubblewrap`."
                .to_string(),
        })
    );
    assert_eq!(
        notice(&RunEventKind::SandboxUnavailable(SandboxWarning::new(
            SandboxUnavailableReason::HelperMissing,
        ))),
        Some(TurnNotice::SandboxUnavailable {
            message: "Commands run without the sandbox. No native sandbox helper is configured."
                .to_string(),
            remedy: "Reinstall or update Agentty, which is missing its sandbox helper.".to_string(),
        })
    );
}

#[test]
/// Reports a likely sandbox denial only for a tool call the harness marked,
/// and nothing for other events.
fn likely_denials_are_reported_only_for_marked_tool_calls() {
    // Arrange
    let tool_call = |likely_sandbox_denial: bool| -> RunEventKind {
        serde_json::from_value(json!({
            "type": "item_completed",
            "id": 1,
            "item": {
                "type": "tool_call",
                "arguments": "{\"command\":\"touch ~/x\"}",
                "call_id": "call-1",
                "likely_sandbox_denial": likely_sandbox_denial,
                "name": "bash",
                "output": "Operation not permitted",
                "status": "completed",
                "summary": ""
            }
        }))
        .expect("tool call event")
    };
    let message: RunEventKind = serde_json::from_value(json!({
        "type": "item_completed",
        "id": 2,
        "item": {"type": "message", "text": "Done."}
    }))
    .expect("message event");

    // Act
    let denied = notice(&tool_call(true));
    let completed = notice(&tool_call(false));
    let other = notice(&message);

    // Assert
    assert_eq!(denied, Some(TurnNotice::LikelySandboxDenial));
    assert_eq!(completed, None);
    assert_eq!(other, None);
}

#[tokio::test]
/// Replaces whatever earlier commands planted in the sandbox `CARGO_HOME`:
/// configuration and credential files of any type, and a `cargo` link
/// leading out of the temporary directory, which it removes without
/// touching the target.
async fn prepare_replaces_planted_cargo_files_and_links() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let data_root = root.path().join("data");
    let temporary = data_root.join("session-1/tmp");
    let cargo_home = temporary.join("cargo");
    std::fs::create_dir_all(cargo_home.join("credentials")).expect("planted directory");
    std::fs::write(
        cargo_home.join("config"),
        "[build]\nrustc-wrapper = \"evil\"\n",
    )
    .expect("planted legacy config");
    std::fs::write(cargo_home.join("config.toml"), "[env]\nEVIL = \"1\"\n")
        .expect("planted config");
    std::os::unix::fs::symlink("/etc/hosts", cargo_home.join("credentials.toml"))
        .expect("planted link");
    std::fs::write(temporary.join(".agentty-discarded-cargo-entry"), "")
        .expect("planted discard entry");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).expect("outside directory");
    std::fs::write(outside.join("config.toml"), "kept").expect("outside file");
    let linked_temporary = data_root.join("session-2/tmp");
    std::fs::create_dir_all(&linked_temporary).expect("second temporary directory");
    std::os::unix::fs::symlink(&outside, linked_temporary.join("cargo")).expect("planted link");
    let filed_temporary = data_root.join("session-3/tmp");
    std::fs::create_dir_all(&filed_temporary).expect("third temporary directory");
    std::fs::write(filed_temporary.join("cargo"), "").expect("planted file");
    let environment = BTreeMap::new();

    // Act
    let planted = per_session(&data_root, temporary.clone(), root.path(), &environment).await;
    let linked = per_session(
        &data_root,
        linked_temporary.clone(),
        root.path(),
        &environment,
    )
    .await;
    let filed = per_session(
        &data_root,
        filed_temporary.clone(),
        root.path(),
        &environment,
    )
    .await;

    // Assert
    assert!(planted.is_ok() && linked.is_ok() && filed.is_ok());
    let left = std::fs::read_dir(&cargo_home)
        .expect("Cargo directory")
        .count();
    assert_eq!(left, 0, "planted files must be gone");
    assert!(std::fs::symlink_metadata(temporary.join(".agentty-discarded-cargo-entry")).is_err());
    for temporary in [linked_temporary, filed_temporary] {
        let metadata = std::fs::symlink_metadata(temporary.join("cargo")).expect("Cargo directory");
        assert!(metadata.is_dir());
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("config.toml")).expect("outside file"),
        "kept"
    );
}

#[test]
/// Clears the Cargo directory that was opened, not whatever a command
/// swaps in at its path afterwards: a link to a directory outside the
/// temporary directory never redirects the removals or the host link.
fn clearing_follows_the_opened_cargo_directory_not_a_swapped_link() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let temporary = root.path().join("tmp");
    let cargo_home = temporary.join("cargo");
    std::fs::create_dir_all(cargo_home.join("credentials")).expect("planted directory");
    std::fs::write(cargo_home.join("config.toml"), "planted").expect("planted config");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(outside.join("credentials")).expect("outside directory");
    std::fs::write(outside.join("config.toml"), "kept").expect("outside file");
    let host_config = root.path().join("host-config.toml");
    let flags =
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW;
    let temporary_handle =
        rustix::fs::open(&temporary, flags, rustix::fs::Mode::empty()).expect("temporary handle");
    let cargo_handle =
        rustix::fs::open(&cargo_home, flags, rustix::fs::Mode::empty()).expect("Cargo handle");
    let opened = temporary.join("opened");
    std::fs::rename(&cargo_home, &opened).expect("move the opened directory");
    std::os::unix::fs::symlink(&outside, &cargo_home).expect("swapped link");

    // Act
    let cleared = CommandSandbox::clear_cargo_home(
        &temporary,
        &temporary_handle,
        &cargo_handle,
        Some(&host_config),
    );

    // Assert
    assert!(cleared.is_ok(), "{cleared:?}");
    assert_eq!(
        std::fs::read_to_string(outside.join("config.toml")).expect("outside file"),
        "kept"
    );
    assert!(outside.join("credentials").is_dir());
    assert!(std::fs::symlink_metadata(opened.join("credentials")).is_err());
    assert_eq!(
        std::fs::read_link(opened.join("config.toml")).expect("host link"),
        host_config
    );
}

#[tokio::test]
/// Reports a temporary directory whose Cargo directory cannot be created.
async fn prepare_reports_an_unusable_temporary_directory() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let temporary = root.path().join("tmp");
    std::fs::write(&temporary, "").expect("file in place of the directory");

    // Act
    let failure = per_session(root.path(), temporary, root.path(), &BTreeMap::new()).await;

    // Assert
    assert!(
        failure
            .err()
            .expect("a file temporary directory should fail")
            .contains("Failed to prepare the sandbox Cargo directory")
    );
}

#[tokio::test]
/// Links the host's Cargo configuration into the sandbox `CARGO_HOME`
/// unless Cargo finds it from the workspace anyway, refreshes the link,
/// and never links what does not exist.
async fn prepare_links_the_host_cargo_configuration() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let data_root = root.path().join("data");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let custom_home = root.path().join("custom-cargo");
    std::fs::create_dir_all(&custom_home).expect("custom Cargo home");
    std::fs::write(custom_home.join("config"), "[net]\n").expect("legacy config");
    let custom = BTreeMap::from([("CARGO_HOME".to_string(), custom_home.display().to_string())]);
    let home_above = BTreeMap::from([("HOME".to_string(), root.path().display().to_string())]);
    std::fs::create_dir_all(root.path().join(".cargo")).expect("default Cargo home");
    std::fs::write(root.path().join(".cargo/config.toml"), "[net]\n").expect("config");
    let unrelated_home = root.path().join("elsewhere");
    let empty_home = BTreeMap::from([("HOME".to_string(), unrelated_home.display().to_string())]);
    let relative = BTreeMap::from([("CARGO_HOME".to_string(), "relative".to_string())]);

    let temporary = data_root.join("session-1/tmp");

    // Act
    per_session(&data_root, temporary.clone(), &workspace, &custom)
        .await
        .expect("linked sandbox");
    let link = temporary.join("cargo/config.toml");
    let linked_target = std::fs::read_link(&link).ok();
    let found = per_session(&data_root, temporary.clone(), &workspace, &home_above).await;
    let after_found = std::fs::symlink_metadata(&link).is_ok();
    let missing = per_session(&data_root, temporary.clone(), &workspace, &empty_home).await;
    let unset = per_session(&data_root, temporary, &workspace, &relative).await;

    // Assert
    assert_eq!(linked_target, Some(custom_home.join("config")));
    assert!(found.is_ok() && missing.is_ok() && unset.is_ok());
    assert!(
        !after_found,
        "a configuration Cargo finds itself is not linked"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
}

#[tokio::test]
/// Hides every project's Cargo cache from commands, which receive copies,
/// creating its root first so that the sandbox masks a real directory.
async fn prepare_hides_the_cargo_cache() {
    // Arrange
    let root = tempfile::tempdir().expect("root");
    let data_root = root.path().join("data");
    let cache = TrustedCargoCache::for_project(&root.path().join("cache"), root.path());

    // Act
    let sandbox = CommandSandbox::prepare(
        &data_root,
        data_root.join("session-1/tmp"),
        root.path(),
        &BTreeMap::new(),
        Some(cache.clone()),
        HostCacheSharing::PerSession,
    )
    .await
    .expect("sandbox");

    // Assert
    assert!(sandbox.hidden.contains(&root.path().join("cache")));
    assert!(
        root.path().join("cache").is_dir(),
        "the hidden cache root exists before any sandbox starts"
    );
    assert_eq!(sandbox.cargo, CargoCacheSource::Project(cache));
}

/// A data root and a fake host home for preparing shared sandboxes.
struct SharedFixture {
    _root: tempfile::TempDir,
    data_root: PathBuf,
    environment: BTreeMap<String, String>,
    home: PathBuf,
    temporary: PathBuf,
    workspace: PathBuf,
}

impl SharedFixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("root");
        let base = root.path().canonicalize().expect("canonical root");
        let data_root = base.join("data");
        let home = base.join("home");
        let workspace = base.join("workspace");
        for directory in [&home, &workspace] {
            std::fs::create_dir_all(directory).expect("fixture directory");
        }
        let environment = BTreeMap::from([
            ("AGENTTY_ROOT".to_string(), "/agentty-root".to_string()),
            ("HOME".to_string(), home.display().to_string()),
        ]);

        Self {
            _root: root,
            temporary: data_root.join("session-1/tmp"),
            data_root,
            environment,
            home,
            workspace,
        }
    }

    async fn prepare(&self, share_host_caches: bool) -> CommandSandbox {
        self.prepare_with(share_host_caches, None).await
    }

    /// Prepares the sandbox with the project's trusted `cargo_cache`.
    async fn prepare_with(
        &self,
        share_host_caches: bool,
        cargo_cache: Option<TrustedCargoCache>,
    ) -> CommandSandbox {
        CommandSandbox::prepare(
            &self.data_root,
            self.temporary.clone(),
            &self.workspace,
            &self.environment,
            cargo_cache,
            HostCacheSharing::from_enabled(share_host_caches),
        )
        .await
        .expect("prepared sandbox")
    }

    /// Returns the policy `sandbox` grants.
    fn granted(sandbox: &CommandSandbox) -> BashConfig {
        sandbox.grant(base_policy()).expect("granted policy")
    }

    /// Returns the policy expected with `caches` shared.
    fn expected(&self, caches: &HostCaches) -> BashConfig {
        expected_shared_policy(
            &self.temporary,
            &self.data_root,
            Path::new("/agentty-root/auth/chatgpt.json"),
            caches,
        )
    }
}

#[tokio::test]
/// Sharing links the sandbox `CARGO_HOME`'s registry, Git dependencies,
/// and cache locks to the host's, creating them there, replacing whatever
/// the session left, and granting exactly them; it never links Cargo's
/// programs, configuration, or credentials. Turning sharing off removes
/// the links and keeps the session's own caches.
async fn sharing_links_the_host_cargo_caches_and_stopping_removes_the_links() {
    // Arrange
    let fixture = SharedFixture::new();
    let cargo = fixture.temporary.join("cargo");
    std::fs::create_dir_all(cargo.join("registry/cache")).expect("session registry");
    std::fs::write(cargo.join("registry/cache/old.crate"), "old").expect("session crate");
    std::os::unix::fs::symlink(&fixture.workspace, cargo.join("git")).expect("planted link");
    std::fs::write(cargo.join(".package-cache"), "").expect("session lock");
    let host_cargo = fixture.home.join(".cargo");

    // Act
    let shared = fixture.prepare(true).await;
    let links = ["registry", "git", ".package-cache", ".package-cache-mutate"]
        .map(|name| std::fs::read_link(cargo.join(name)).ok());
    let shared_policy = SharedFixture::granted(&shared);
    let isolated = fixture.prepare(false).await;
    let after_stopping = ["registry", "git", ".package-cache", ".package-cache-mutate"]
        .map(|name| std::fs::symlink_metadata(cargo.join(name)).is_ok());
    std::fs::create_dir(cargo.join("registry")).expect("session registry again");
    fixture.prepare(false).await;

    // Assert
    assert_eq!(
        links,
        ["registry", "git", ".package-cache", ".package-cache-mutate"]
            .map(|name| Some(host_cargo.join(name)))
    );
    assert!(host_cargo.join("registry").is_dir() && host_cargo.join("git").is_dir());
    assert!(host_cargo.join(".package-cache").is_file());
    assert!(!host_cargo.join("bin").exists() && !host_cargo.join("config.toml").exists());
    assert!(
        std::fs::symlink_metadata(fixture.temporary.join(".agentty-discarded-cargo-entry"))
            .is_err()
    );
    assert!(shared.shares_host_caches());
    assert_eq!(
        shared_policy,
        fixture.expected(&HostCaches {
            directories: vec![host_cargo.join("registry"), host_cargo.join("git")],
            files: vec![
                host_cargo.join(".package-cache"),
                host_cargo.join(".package-cache-mutate")
            ],
            variables: BTreeMap::new(),
        })
    );
    assert_eq!(shared.notice(), None);
    assert!(!isolated.shares_host_caches());
    assert_eq!(isolated.notice(), None);
    assert_eq!(after_stopping, [false; 4]);
    assert_eq!(
        SharedFixture::granted(&isolated),
        fixture.expected(&HostCaches::default())
    );
    assert!(
        cargo.join("registry").is_dir(),
        "a session's own registry stays when not sharing"
    );
}

#[tokio::test]
/// Sharing grants the Go, pip, and npm caches that exist on the host and
/// points their variables there, keeps the missing ones and
/// `XDG_CACHE_HOME` per session, and keeps Cargo per session when the
/// host's Cargo caches are links or its Cargo home cannot be created.
async fn sharing_grants_existing_tool_caches_and_refuses_linked_cargo_caches() {
    // Arrange
    let mut fixture = SharedFixture::new();
    let mod_cache = fixture.home.join("go/pkg/mod");
    let npm_cache = fixture.home.join(".npm");
    for directory in [&mod_cache, &npm_cache, &fixture.home.join("elsewhere")] {
        std::fs::create_dir_all(directory).expect("host cache");
    }
    std::fs::create_dir_all(fixture.home.join(".cargo")).expect("host Cargo home");
    std::os::unix::fs::symlink(
        fixture.home.join("elsewhere"),
        fixture.home.join(".cargo/registry"),
    )
    .expect("linked registry");

    // Act
    let linked = fixture.prepare(true).await;
    let linked_policy = SharedFixture::granted(&linked);
    std::fs::write(fixture.home.join("file"), "").expect("file");
    fixture.environment.insert(
        "CARGO_HOME".to_string(),
        fixture.home.join("file/cargo").display().to_string(),
    );
    let uncreatable = fixture.prepare(true).await;

    // Assert
    let tools = HostCaches {
        directories: vec![mod_cache.clone(), npm_cache.clone()],
        files: Vec::new(),
        variables: BTreeMap::from([("GOMODCACHE", mod_cache), ("npm_config_cache", npm_cache)]),
    };
    assert_eq!(linked_policy, fixture.expected(&tools));
    assert_eq!(
        SharedFixture::granted(&uncreatable),
        fixture.expected(&tools)
    );
    let cargo_kept = Some(TurnNotice::HostCachesNotShared {
        caches: vec!["Cargo".to_string()],
    });
    assert_eq!(linked.notice(), cargo_kept);
    assert_eq!(uncreatable.notice(), cargo_kept);
    assert!(std::fs::symlink_metadata(fixture.temporary.join("cargo/registry")).is_err());
    assert!(!fixture.home.join(".cargo/git").exists());
}

#[tokio::test]
/// Keeps per session, and names, every cache the native sandbox would
/// refuse, so sharing never makes every command fail: a Cargo home outside
/// the home directory, such as a system-wide `/usr/local/cargo`, a lock
/// file with another link, and tool caches that are links, lie on `PATH`,
/// or are files.
async fn sharing_keeps_caches_the_sandbox_would_refuse_per_session() {
    // Arrange
    let mut fixture = SharedFixture::new();
    let outside_home = fixture.data_root.with_file_name("system-cargo");
    std::fs::create_dir_all(&outside_home).expect("Cargo home outside the home directory");
    let host_cargo = fixture.home.join(".cargo");
    std::fs::create_dir_all(host_cargo.join("registry")).expect("host registry");
    std::fs::write(host_cargo.join(".package-cache"), "").expect("host lock");
    std::fs::hard_link(
        host_cargo.join(".package-cache"),
        fixture.home.join("backup-lock"),
    )
    .expect("hard-linked lock");
    std::fs::create_dir_all(fixture.home.join("elsewhere")).expect("link target");
    std::os::unix::fs::symlink(fixture.home.join("elsewhere"), fixture.home.join(".npm"))
        .expect("linked npm cache");
    std::fs::create_dir_all(fixture.home.join("go/pkg/mod")).expect("Go module cache");
    std::fs::create_dir_all(fixture.home.join("Library/Caches")).expect("macOS cache root");
    std::fs::create_dir_all(fixture.home.join(".cache")).expect("Linux cache root");
    std::fs::write(fixture.home.join("Library/Caches/pip"), "").expect("pip file");
    std::fs::write(fixture.home.join(".cache/pip"), "").expect("pip file");
    fixture.environment.insert(
        "PATH".to_string(),
        fixture.home.join("go").display().to_string(),
    );

    // Act
    let hard_linked = fixture.prepare(true).await;
    fixture
        .environment
        .insert("CARGO_HOME".to_string(), outside_home.display().to_string());
    let outside = fixture.prepare(true).await;

    // Assert
    for sandbox in [&hard_linked, &outside] {
        assert!(!sandbox.shares_host_caches());
        assert_eq!(
            sandbox.notice(),
            Some(TurnNotice::HostCachesNotShared {
                caches: ["Cargo", "Go modules", "pip", "npm"]
                    .map(String::from)
                    .to_vec(),
            })
        );
        assert_eq!(
            SharedFixture::granted(sandbox),
            fixture.expected(&HostCaches::default())
        );
    }
    assert!(std::fs::symlink_metadata(fixture.temporary.join("cargo/registry")).is_err());
    assert!(!outside_home.join("registry").exists());
}

#[test]
/// Finds each tool's cache where the host keeps it: the variable that moves
/// it when absolute, else the tool's default for the platform.
fn host_tool_caches_follow_each_tools_documented_defaults() {
    // Arrange
    let home = BTreeMap::from([("HOME".to_string(), "/home/user".to_string())]);
    let moved = BTreeMap::from([
        ("HOME".to_string(), "/home/user".to_string()),
        ("XDG_CACHE_HOME".to_string(), "/xdg".to_string()),
        ("GOPATH".to_string(), "/gopath:/second".to_string()),
        ("NPM_CONFIG_CACHE".to_string(), "/npm-upper".to_string()),
    ]);
    let explicit = BTreeMap::from([
        ("GOMODCACHE".to_string(), "/mod".to_string()),
        ("GOCACHE".to_string(), "/build".to_string()),
        ("GOPATH".to_string(), "relative".to_string()),
        ("PIP_CACHE_DIR".to_string(), "/pip".to_string()),
        ("npm_config_cache".to_string(), "/npm".to_string()),
        ("NPM_CONFIG_CACHE".to_string(), "/npm-upper".to_string()),
        ("XDG_CACHE_HOME".to_string(), "relative".to_string()),
    ]);
    let caches = |pairs: [(&'static str, &str); 4]| {
        pairs
            .map(|(variable, path)| (variable, PathBuf::from(path)))
            .to_vec()
    };
    let found = |environment: &BTreeMap<String, String>, platform: &str| {
        CommandSandbox::host_tool_caches(environment, platform)
            .into_iter()
            .map(|(variable, _, path)| (variable, path))
            .collect::<Vec<_>>()
    };

    // Act / Assert
    assert_eq!(
        CommandSandbox::host_tool_caches(&home, "linux")
            .iter()
            .map(|(_, name, _)| *name)
            .collect::<Vec<_>>(),
        ["Go modules", "Go build", "pip", "npm"]
    );
    assert_eq!(
        found(&home, "macos"),
        caches([
            ("GOMODCACHE", "/home/user/go/pkg/mod"),
            ("GOCACHE", "/home/user/Library/Caches/go-build"),
            ("PIP_CACHE_DIR", "/home/user/Library/Caches/pip"),
            ("npm_config_cache", "/home/user/.npm"),
        ])
    );
    assert_eq!(
        found(&home, "linux"),
        caches([
            ("GOMODCACHE", "/home/user/go/pkg/mod"),
            ("GOCACHE", "/home/user/.cache/go-build"),
            ("PIP_CACHE_DIR", "/home/user/.cache/pip"),
            ("npm_config_cache", "/home/user/.npm"),
        ])
    );
    assert_eq!(
        found(&moved, "linux"),
        caches([
            ("GOMODCACHE", "/gopath/pkg/mod"),
            ("GOCACHE", "/xdg/go-build"),
            ("PIP_CACHE_DIR", "/xdg/pip"),
            ("npm_config_cache", "/npm-upper"),
        ])
    );
    assert_eq!(
        found(&moved, "macos")[1..3],
        caches([
            ("GOMODCACHE", ""),
            ("GOCACHE", "/home/user/Library/Caches/go-build"),
            ("PIP_CACHE_DIR", "/xdg/pip"),
            ("npm_config_cache", ""),
        ])[1..3]
    );
    assert_eq!(
        found(&explicit, "linux"),
        caches([
            ("GOMODCACHE", "/mod"),
            ("GOCACHE", "/build"),
            ("PIP_CACHE_DIR", "/pip"),
            ("npm_config_cache", "/npm"),
        ])
    );
    assert_eq!(found(&BTreeMap::new(), "linux"), Vec::new());
}

#[tokio::test]
/// Sharing the host's Cargo caches takes the place of the project's
/// trusted cache, which stays hidden and copies nothing into the linked
/// registry; when the host's Cargo caches cannot be shared, the session
/// uses the project's cache instead and the notice names Cargo.
async fn host_cargo_sharing_wins_over_the_project_cache_and_falls_back_to_it() {
    // Arrange
    let mut fixture = SharedFixture::new();
    let cache_root = fixture.data_root.with_file_name("cache");
    let cache = TrustedCargoCache::for_project(&cache_root, &fixture.workspace);
    let host_cargo = fixture.home.join(".cargo");

    // Act
    let shared = fixture.prepare_with(true, Some(cache.clone())).await;
    let linked = std::fs::read_link(fixture.temporary.join("cargo/registry")).ok();
    fixture.environment.insert(
        "CARGO_HOME".to_string(),
        fixture
            .data_root
            .with_file_name("system-cargo")
            .display()
            .to_string(),
    );
    let refused = fixture.prepare_with(true, Some(cache.clone())).await;

    // Assert
    assert_eq!(shared.cargo, CargoCacheSource::Host(host_cargo.clone()));
    assert_eq!(linked, Some(host_cargo.join("registry")));
    assert!(shared.hidden.contains(&cache_root));
    assert_eq!(shared.notice(), None);
    assert_eq!(refused.cargo, CargoCacheSource::Project(cache));
    assert!(refused.hidden.contains(&cache_root));
    assert!(std::fs::symlink_metadata(fixture.temporary.join("cargo/registry")).is_err());
    assert_eq!(
        refused.notice(),
        Some(TurnNotice::HostCachesNotShared {
            caches: vec!["Cargo".to_string()],
        })
    );
}

#[tokio::test]
/// Shares Cargo's caches although Cargo's `bin` is on `PATH`, grants a tool
/// cache by its resolved path, and keeps per session, and names, the tool
/// caches the native sandbox's write-grant rules would refuse: one under
/// the home's `Library`, one inside a cache already shared, and one inside
/// a hidden path, the project's trusted Cargo cache.
async fn sharing_keeps_library_and_overlapping_caches_per_session() {
    // Arrange
    let mut fixture = SharedFixture::new();
    let mod_cache = fixture.home.join("go/pkg/mod");
    let build_cache = fixture.home.join("Library/Caches/go-build");
    let pip_cache = mod_cache.join("pip");
    let cache_root = fixture.home.join("trusted");
    let npm_cache = cache_root.join("npm");
    for directory in [&mod_cache, &build_cache, &pip_cache, &npm_cache] {
        std::fs::create_dir_all(directory).expect("host cache");
    }
    std::os::unix::fs::symlink(fixture.home.join("go"), fixture.home.join("go-alias"))
        .expect("linked ancestor");
    for (variable, path) in [
        ("GOMODCACHE", fixture.home.join("go-alias/pkg/mod")),
        ("GOCACHE", build_cache),
        ("PIP_CACHE_DIR", pip_cache),
        ("npm_config_cache", npm_cache),
        ("PATH", fixture.home.join(".cargo/bin")),
    ] {
        fixture
            .environment
            .insert(variable.to_string(), path.display().to_string());
    }
    let cache = TrustedCargoCache::for_project(&cache_root, &fixture.workspace);

    // Act
    let sandbox = fixture.prepare_with(true, Some(cache)).await;

    // Assert
    assert_eq!(
        sandbox.host_cache_directories,
        [
            fixture.home.join(".cargo/registry"),
            fixture.home.join(".cargo/git"),
            mod_cache.clone(),
        ]
    );
    assert_eq!(
        sandbox.shared_environment.get("GOMODCACHE"),
        Some(&mod_cache)
    );
    assert_eq!(
        sandbox.notice(),
        Some(TurnNotice::HostCachesNotShared {
            caches: ["Go build", "pip", "npm"].map(String::from).to_vec(),
        })
    );
}

#[test]
/// Refuses a cache with a Git metadata component, whatever its case.
fn share_limits_refuse_git_metadata_components() {
    // Arrange
    let environment = BTreeMap::from([("HOME".to_string(), "/home/user".to_string())]);
    let limits = ShareLimits::new(&environment, &[]);

    // Act / Assert
    assert!(limits.allows(Path::new("/home/user/cache")));
    assert!(!limits.allows(Path::new("/home/user/repository/.GIT/cache")));
}
