//! Native sandbox grant and notice unit tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ag_contracts::TurnNotice;
use ag_harness::RunEventKind;
use ag_harness::bash::BashConfig;
use ag_harness::sandbox::{IfUnavailable, Sandbox, SandboxUnavailableReason, SandboxWarning};
use serde_json::json;

use super::{CommandSandbox, SANDBOX, notice};

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
/// the hidden `data_root` and `auth_file`.
fn expected_policy(temporary: &Path, data_root: &Path, auth_file: &Path) -> BashConfig {
    let mut expected = base_policy()
        .with_full_disk_read()
        .and_then(|config| config.with_write(PathBuf::from(".")))
        .map(BashConfig::with_git_writes)
        .map(BashConfig::with_network)
        .and_then(|config| config.with_temporary_directory(temporary.to_path_buf()))
        .and_then(|config| config.with_hidden_path(data_root.to_path_buf()))
        .and_then(|config| config.with_hidden_path(auth_file.to_path_buf()))
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
/// Runs commands in the native sandbox and without it, with a warning,
/// where it cannot run.
fn sandbox_is_native_with_an_unsandboxed_fallback() {
    // Arrange, Act, Assert
    assert_eq!(
        SANDBOX,
        Sandbox::Native {
            if_unavailable: IfUnavailable::Unsandboxed
        }
    );
}

#[test]
/// Grants full-disk reads, worktree, Git, and temporary-directory writes,
/// the network, and caches in the temporary directory, and hides the data
/// root and the `ChatGPT` sign-in, with no writable roots or excluded
/// commands.
fn grant_adds_the_coding_agent_grant_set() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let temporary = data_root.path().join("session-1").join("tmp");
    let environment = BTreeMap::from([("AGENTTY_ROOT".to_string(), "/agentty-root".to_string())]);
    let sandbox = CommandSandbox::new(data_root.path(), temporary.clone(), &environment);

    // Act
    let granted = sandbox.grant(base_policy(), &environment);

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
/// Grants the SSH agent's socket only where the sandbox can allow one by
/// path, and skips a socket that does not exist.
fn grant_allows_the_ssh_agent_socket_on_macos() {
    // Arrange
    let directory = tempfile::tempdir().expect("socket directory");
    let socket = directory.path().join("agent.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("socket");
    let temporary = directory.path().join("tmp");
    let environment = |socket: &Path| {
        BTreeMap::from([
            ("AGENTTY_ROOT".to_string(), "/agentty-root".to_string()),
            ("SSH_AUTH_SOCK".to_string(), socket.display().to_string()),
        ])
    };
    let sandbox = CommandSandbox::new(
        Path::new("/data-root"),
        temporary.clone(),
        &environment(&socket),
    );
    let without_socket = expected_policy(
        &temporary,
        Path::new("/data-root"),
        Path::new("/agentty-root/auth/chatgpt.json"),
    );

    // Act
    let granted = sandbox.grant(base_policy(), &environment(&socket));
    let missing = sandbox.grant(
        base_policy(),
        &environment(&directory.path().join("missing.sock")),
    );

    // Assert
    #[cfg(target_os = "macos")]
    let expected = without_socket
        .clone()
        .with_unix_socket(socket)
        .expect("socket grant");
    #[cfg(not(target_os = "macos"))]
    let expected = without_socket.clone();
    assert_eq!(granted.expect("granted policy"), expected);
    assert_eq!(missing.expect("granted policy"), without_socket);
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
