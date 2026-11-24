//! Native sandbox grant and notice unit tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::BashConfig;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use serde_json::json;

use super::{CommandSandbox, SESSION_SANDBOX, UTILITY_SANDBOX, notice};

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

/// Returns the policy Agentty's grants must produce for `temporary`, the
/// session branch `branch`, and the hidden `data_root` and `auth_file`,
/// with the stored API keys, application database and pasted images of the
/// Agentty root `/agentty-root` hidden too.
fn expected_policy(
    temporary: &Path,
    branch: Option<&str>,
    data_root: &Path,
    auth_file: &Path,
) -> BashConfig {
    let mut expected = base_policy()
        .with_full_disk_read()
        .and_then(|config| config.with_write(PathBuf::from(".")))
        .and_then(|config| match branch {
            Some(branch) => config.with_git_writes(branch.to_string()),
            None => Ok(config),
        })
        .map(BashConfig::with_network)
        .and_then(|config| config.with_temporary_directory(temporary.to_path_buf()))
        .and_then(|config| config.with_hidden_path(data_root.to_path_buf()))
        .and_then(|config| config.with_hidden_path(auth_file.to_path_buf()))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/auth")))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/db")))
        .and_then(|config| config.with_hidden_path(PathBuf::from("/agentty-root/tmp")))
        .expect("expected grants");
    for (name, directory) in [
        ("CARGO_HOME", "cargo"),
        ("GOCACHE", "go-build"),
        ("GOMODCACHE", "go-mod"),
        ("PIP_CACHE_DIR", "pip"),
        ("XDG_CACHE_HOME", "cache"),
        ("npm_config_cache", "npm"),
    ] {
        let value = temporary.join(directory).display().to_string();
        expected = expected
            .with_environment(name.to_string(), value)
            .expect("cache variable");
    }

    expected
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
/// Grants full-disk reads, worktree, session-branch, and
/// temporary-directory writes, the network, and caches in the temporary
/// directory, and hides the data root and the `ChatGPT` sign-in, with no
/// writable roots, sockets (not even an SSH agent's), or excluded commands;
/// without a session branch, no Git writes.
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
    let auth_file = Path::new("/agentty-root/auth/chatgpt.json");

    // Act
    let without_branch = sandbox.grant(base_policy());
    let sandbox = sandbox.with_git_branch(Some("wt/session".to_string()));
    let granted = sandbox.grant(base_policy());
    let invalid = sandbox
        .with_git_branch(Some("wt/..".to_string()))
        .grant(base_policy());

    // Assert
    assert_eq!(
        granted.expect("granted policy"),
        expected_policy(&temporary, Some("wt/session"), data_root.path(), auth_file)
    );
    assert_eq!(
        without_branch.expect("policy without a branch"),
        expected_policy(&temporary, None, data_root.path(), auth_file)
    );
    assert!(invalid.is_err());
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
    let planted =
        CommandSandbox::prepare(&data_root, temporary.clone(), root.path(), &environment).await;
    let linked = CommandSandbox::prepare(
        &data_root,
        linked_temporary.clone(),
        root.path(),
        &environment,
    )
    .await;
    let filed = CommandSandbox::prepare(
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
    let failure =
        CommandSandbox::prepare(root.path(), temporary, root.path(), &BTreeMap::new()).await;

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
    CommandSandbox::prepare(&data_root, temporary.clone(), &workspace, &custom)
        .await
        .expect("linked sandbox");
    let link = temporary.join("cargo/config.toml");
    let linked_target = std::fs::read_link(&link).ok();
    let found =
        CommandSandbox::prepare(&data_root, temporary.clone(), &workspace, &home_above).await;
    let after_found = std::fs::symlink_metadata(&link).is_ok();
    let missing =
        CommandSandbox::prepare(&data_root, temporary.clone(), &workspace, &empty_home).await;
    let unset = CommandSandbox::prepare(&data_root, temporary, &workspace, &relative).await;

    // Assert
    assert_eq!(linked_target, Some(custom_home.join("config")));
    assert!(found.is_ok() && missing.is_ok() && unset.is_ok());
    assert!(
        !after_found,
        "a configuration Cargo finds itself is not linked"
    );
    assert!(std::fs::symlink_metadata(&link).is_err());
}
