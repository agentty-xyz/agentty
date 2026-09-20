#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ag_harness::{BashConfig, CommandOutcome, ToolPolicy, TurnError, TurnLimits, TurnOptions};

use super::fixture::{NativeFixture, Workspace, schema, with_runtime};

#[cfg(target_os = "linux")]
#[tokio::test]
async fn native_linux_denies_keyrings_and_nested_user_namespaces() {
    // Arrange
    let workspace = Workspace::new();
    let fixture = NativeFixture::build();

    // Act
    let result = workspace
        .harness()
        .run_once_with_options(
            format!(
                "'{}' keyring && '{}' namespace",
                fixture.executable().display(),
                fixture.executable().display()
            ),
            fixture.options(&workspace),
        )
        .await
        .expect("turn");
    let result: CommandOutcome = serde_json::from_value(result.into_output()).expect("outcome");

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(
        result.cleanup_scope,
        ag_harness::CommandCleanupScope::PidNamespace
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn native_linux_rejects_directory_write_grants_before_execution() {
    // Arrange
    let workspace = Workspace::new();
    let options = workspace.options(Duration::from_secs(5), 128);
    let configuration = options
        .bash()
        .expect("policy")
        .clone()
        .with_write("output".into())
        .expect("write grant");

    // Act
    let result = workspace
        .harness()
        .run_once_with_options(
            "printf escaped > output/escaped",
            options.with_bash(configuration),
        )
        .await;

    // Assert
    let Err(TurnError::CommandFailed { outcome }) = result else {
        std::panic::resume_unwind(Box::new("write grants must fail closed before execution"));
    };
    assert_eq!(
        outcome.execution_failure,
        Some(ag_harness::BashError::Unavailable)
    );
    assert!(!workspace.path().join("output/escaped").exists());
}

#[tokio::test]
async fn native_positive_controls_and_filesystem_network_denials() {
    // Arrange
    let workspace = Workspace::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("address").port();
    let connection = std::net::TcpStream::connect(listener.local_addr().expect("address"))
        .expect("positive network control");
    drop(connection);

    // Act
    #[cfg(target_os = "macos")]
    let write_control = "printf allowed > output/file";
    #[cfg(target_os = "linux")]
    let write_control = "if printf forbidden > output/file; then exit 94; fi";
    let result = workspace
        .run(&format!(
            "set -e; /bin/cat input; {write_control}; if printf forbidden > input; then exit 91; \
             fi; if printf forbidden > .git/config; then exit 92; fi; if printf forbidden > \
             /dev/tcp/127.0.0.1/{port}; then exit 93; fi; printf confined"
        ))
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "input-valueconfined");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("input")).expect("input"),
        "input-value"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(".git/config")).expect("metadata"),
        "protected"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("output/file")).expect("write"),
        "allowed"
    );
    #[cfg(target_os = "linux")]
    assert!(!workspace.path().join("output/file").exists());
    assert!(!result.cleanup_failed);
    assert!(!result.stderr.contains("LLVM Profile"), "{result:?}");
    workspace.assert_launcher_profiles();
}

#[tokio::test]
async fn native_denied_tool_and_unsafe_aliases_never_execute() {
    // Arrange
    let workspace = Workspace::new();
    let options = TurnOptions::new(schema(), ToolPolicy::default(), TurnLimits::default());
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret"), "secret").expect("secret");

    // Act
    let denied = workspace
        .harness()
        .run_once_with_options("printf escaped > output/escaped", options)
        .await;
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("alias")).expect("alias");
    let alias = workspace
        .harness()
        .run_once_with_options(
            "printf escaped > output/escaped",
            workspace.options(Duration::from_secs(5), 128),
        )
        .await;

    // Assert
    assert!(matches!(denied, Err(TurnError::ToolDenied { .. })));
    let Err(TurnError::CommandFailed { outcome }) = alias else {
        std::panic::resume_unwind(Box::new("unsafe aliases must fail before execution"));
    };
    assert_eq!(
        outcome.execution_failure,
        Some(ag_harness::BashError::Unavailable)
    );
    assert!(!workspace.path().join("output/escaped").exists());
}

#[tokio::test]
async fn native_rejects_hardlinks_ipc_nodes_and_missing_enforcement_before_execution() {
    // Arrange
    for unsafe_kind in ["hardlink", "socket", "launcher"] {
        let workspace = Workspace::new();
        let mut options = workspace.options(Duration::from_secs(5), 128);
        let socket = match unsafe_kind {
            "hardlink" => {
                std::fs::hard_link(
                    workspace.path().join("input"),
                    workspace.path().join("output/alias"),
                )
                .expect("hardlink");
                None
            }
            "socket" => Some(
                std::os::unix::net::UnixListener::bind(workspace.path().join("service"))
                    .expect("host IPC"),
            ),
            _ => {
                let configuration = BashConfig::new(
                    workspace.path().join("missing-launcher"),
                    "/bin/bash".into(),
                    "missing".into(),
                    Duration::from_secs(5),
                    128,
                )
                .expect("policy")
                .with_host_information()
                .with_read("/bin".into())
                .expect("runtime");
                options = options.with_bash(configuration);
                None
            }
        };

        // Act
        let result = workspace
            .harness()
            .run_once_with_options("printf escaped > output/escaped", options)
            .await;

        // Assert
        assert!(
            matches!(result, Err(TurnError::CommandFailed { .. })),
            "{result:?}"
        );
        assert!(!workspace.path().join("output/escaped").exists());
        drop(socket);
    }
}

#[tokio::test]
async fn native_external_contents_require_a_read_grant_and_precancel_never_spawns() {
    // Arrange
    let workspace = Workspace::new();
    let outside = tempfile::tempdir().expect("outside");
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "read-positive-control").expect("secret");
    let secret = secret.canonicalize().expect("canonical external grant");
    let command = format!("/bin/cat '{}'", secret.display());
    let options = workspace.options(Duration::from_secs(5), 128);
    let configuration = options
        .bash()
        .expect("policy")
        .clone()
        .with_read(secret)
        .expect("read grant");
    let harness = workspace.harness();
    let cancelled = harness.run_once_controlled("printf escaped > output/escaped", options.clone());
    let control = cancelled.control();
    control.cancel();

    // Act
    let cancelled = cancelled.await;
    control
        .commands_settled()
        .await
        .expect("no command effects");
    let denied = harness
        .run_once_with_options(command.as_str(), options.clone())
        .await
        .expect("denied command result");
    let permitted = harness
        .run_once_with_options(command.as_str(), options.with_bash(configuration))
        .await
        .expect("granted command result");
    let denied: CommandOutcome =
        serde_json::from_value(denied.into_output()).expect("denied outcome");
    let permitted: CommandOutcome =
        serde_json::from_value(permitted.into_output()).expect("permitted outcome");

    // Assert
    assert!(matches!(cancelled, Err(TurnError::Cancelled)));
    assert!(!workspace.path().join("output/escaped").exists());
    assert_ne!(denied.exit_code, Some(0));
    assert_eq!(denied.stdout, "");
    assert_eq!(permitted.exit_code, Some(0), "{permitted:?}");
    assert_eq!(permitted.stdout, "read-positive-control", "{permitted:?}");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_linked_worktree_administration_stays_read_only_inside_a_write_grant() {
    // Arrange
    let workspace = Workspace::new();
    let root = workspace.path();
    std::fs::remove_dir_all(root.join(".git")).expect("replace fixture metadata");
    std::fs::create_dir(root.join("output/admin")).expect("worktree administration");
    std::fs::create_dir(root.join("output/common")).expect("common administration");
    std::fs::write(root.join(".git"), "gitdir: output/admin\n").expect("gitdir pointer");
    std::fs::write(root.join("output/admin/commondir"), "../common\n").expect("common pointer");
    std::fs::write(root.join("output/common/config"), "protected").expect("config");

    // Act
    let result = workspace
        .run(
            "if printf forbidden > output/admin/commondir; then exit 91; fi; if printf forbidden \
             > output/common/config; then exit 92; fi; printf allowed > output/ordinary",
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("output/common/config")).expect("config"),
        "protected"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("output/ordinary")).expect("ordinary output"),
        "allowed"
    );
}

#[tokio::test]
async fn native_inherited_descriptors_and_environment_are_not_command_capabilities() {
    // Arrange
    let workspace = Workspace::new();
    let fixture = NativeFixture::build();
    let secret = tempfile::tempfile().expect("host descriptor");
    rustix::io::fcntl_setfd(&secret, rustix::io::FdFlags::empty())
        .expect("inheritable positive control");
    let descriptor = std::os::fd::AsRawFd::as_raw_fd(&secret).to_string();
    let positive = tokio::process::Command::new(fixture.executable())
        .args(["fd", &descriptor])
        .status()
        .await
        .expect("positive descriptor control");
    let options = fixture.options(&workspace);
    let configuration = options
        .bash()
        .expect("policy")
        .clone()
        .with_environment("VISIBLE".into(), "granted".into())
        .expect("environment grant");

    // Act
    let result = workspace
        .harness()
        .run_once_with_options(
            format!(
                "'{}' fd {descriptor}; status=$?; printf '%s|%s|%s' \"$status\" \"$VISIBLE\" \
                 \"${{HOME-unset}}\"",
                fixture.executable().display()
            ),
            options.with_bash(configuration),
        )
        .await
        .expect("turn");
    let result: CommandOutcome = serde_json::from_value(result.into_output()).expect("outcome");

    // Assert
    assert_eq!(positive.code(), Some(1));
    assert_eq!(result.stdout, "0|granted|unset", "{result:?}");
}

#[tokio::test]
async fn native_loader_environment_applies_only_after_isolation() {
    // Arrange
    let workspace = Workspace::new();
    let fixture = NativeFixture::build();
    let library = fixture.library();
    let outside = tempfile::tempdir().expect("outside");
    let forbidden = outside.path().join("forbidden");
    let marker = workspace.path().join("output/loader");
    let variable = if cfg!(target_os = "macos") {
        "DYLD_INSERT_LIBRARIES"
    } else {
        "LD_PRELOAD"
    };
    let environment = [
        (variable, library.to_str().expect("library")),
        ("FORBIDDEN_PATH", forbidden.to_str().expect("forbidden")),
        ("MARKER_PATH", marker.to_str().expect("marker")),
    ];
    let positive = tokio::process::Command::new(fixture.executable())
        .args(["--noprofile", "--norc", "-c", "unused"])
        .envs(environment)
        .status()
        .await
        .expect("loader positive control");
    assert!(positive.success());
    assert!(
        forbidden.exists(),
        "fixture must load and execute the constructor"
    );
    std::fs::remove_file(&forbidden).expect("reset positive control");
    std::fs::remove_file(&marker).expect("reset marker");
    let options = workspace.options(Duration::from_secs(10), 128);
    let configuration = BashConfig::new(
        workspace.launcher(),
        fixture.executable(),
        "loader-fixture".into(),
        Duration::from_secs(10),
        128,
    )
    .expect("configuration")
    .with_host_information();
    #[cfg(target_os = "macos")]
    let configuration = configuration
        .with_write("output".into())
        .expect("write grant");
    let mut configuration = with_runtime(configuration, &[fixture.executable(), library.clone()]);
    for (name, value) in environment {
        configuration = configuration
            .with_environment(name.into(), value.into())
            .expect("environment");
    }

    // Act
    let result = workspace
        .harness()
        .run_once_with_options("unused", options.with_bash(configuration))
        .await
        .expect("turn");
    let result: CommandOutcome = serde_json::from_value(result.into_output()).expect("outcome");

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(!forbidden.exists(), "loader must inherit isolation");
    // The read-only Linux policy leaves the marker unwritable, so the
    // constructor reports through captured stdout instead.
    #[cfg(target_os = "macos")]
    assert_eq!(
        std::fs::read_to_string(marker).expect("constructor executed"),
        "confined"
    );
    #[cfg(target_os = "linux")]
    {
        assert!(!marker.exists());
        assert_eq!(result.stdout, "confined", "{result:?}");
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_metadata_denials_include_new_names_and_nested_repository_scope() {
    // Arrange
    let workspace = Workspace::new();
    let root = workspace.path();
    std::fs::create_dir_all(root.join("output/nested/.git")).expect("nested metadata");
    std::fs::write(root.join("output/nested/.git/config"), "protected").expect("nested config");
    std::fs::create_dir(root.join("output/new-directory")).expect("new metadata parent");
    std::fs::create_dir(root.join("output/new-file")).expect("new gitfile parent");

    // Act
    let outcome = workspace
        .run(
            "if printf changed > output/nested/.git/config; then exit 91; fi; if /bin/mkdir \
             output/new-directory/.git; then exit 92; fi; if /bin/mkdir \
             output/new-directory/.GiT; then exit 96; fi; if printf pointer > \
             output/new-file/.git; then exit 93; fi; printf allowed > output/ordinary",
        )
        .await;
    let nested = ag_harness::Harness::new(super::fixture::ShellModel).repository(
        ag_harness::Repository::new(root.join("output/nested"), "/usr/bin/git")
            .expect("nested repository"),
    );
    let options = workspace.options(Duration::from_secs(10), 128);
    std::fs::create_dir(root.join("output/nested/output")).expect("nested output");
    let nested_coverage = super::coverage::Coverage::new(&root.join("output/nested"));
    let nested_launcher = nested_coverage.as_ref().map_or_else(
        super::coverage::launcher,
        super::coverage::Coverage::launcher,
    );
    let configuration = BashConfig::new(
        nested_launcher,
        "/bin/bash".into(),
        "nested".into(),
        Duration::from_secs(10),
        128,
    )
    .expect("configuration")
    .with_host_information()
    .with_write(".".into())
    .expect("nested grant");
    let nested_result = nested
        .run_once_with_options(
            "if printf changed > .git/config; then exit 94; fi; /bin/mkdir fresh; if /bin/mkdir \
             fresh/.git; then exit 95; fi; printf allowed > ordinary",
            options.with_bash(with_runtime(configuration, &[])),
        )
        .await
        .expect("nested turn");
    let nested_result: CommandOutcome =
        serde_json::from_value(nested_result.into_output()).expect("nested outcome");

    // Assert
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    assert_eq!(nested_result.exit_code, Some(0), "{nested_result:?}");
    assert_eq!(
        std::fs::read_to_string(root.join("output/nested/.git/config")).expect("config"),
        "protected"
    );
    for path in [
        "output/new-directory/.git",
        "output/new-directory/.GiT",
        "output/new-file/.git",
        "output/nested/fresh/.git",
    ] {
        assert!(
            !root.join(path).exists(),
            "new metadata must be denied: {path}"
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_alias_created_by_another_command_after_validation_cannot_expose_metadata() {
    // Arrange
    let workspace = Workspace::new();
    let root = workspace.path();
    std::fs::create_dir(root.join("output/scope")).expect("grant");
    let gate = tempfile::tempdir().expect("trusted launcher gate");
    let gate_root = gate.path().canonicalize().expect("gate root");
    let launcher = gate_root.join("launcher");
    let ready = gate_root.join("ready");
    let release = gate_root.join("release");
    let delegate = workspace.launcher();
    std::fs::write(
        &launcher,
        format!(
            "#!/bin/bash\nif [ \"${{1-}}\" != --seatbelt-child ]; then printf ready > '{}'; while \
             [ ! -e '{}' ]; do /bin/sleep 0.01; done; fi\nexec '{}' \"$@\"\n",
            ready.display(),
            release.display(),
            delegate.display()
        ),
    )
    .expect("gate source");
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700))
        .expect("gate executable");
    let configuration = BashConfig::new(
        launcher,
        "/bin/bash".into(),
        "alias-race".into(),
        Duration::from_secs(10),
        128,
    )
    .expect("configuration")
    .with_host_information()
    .with_write("output/scope".into())
    .expect("scope grant");
    let configuration = if root.join("output/coverage").exists() {
        configuration
            .with_write("output/coverage".into())
            .expect("launcher profiling")
    } else {
        configuration
    };
    let options = workspace
        .options(Duration::from_secs(10), 128)
        .with_bash(with_runtime(configuration, &[delegate]));
    let harness = workspace.harness();
    let turn = harness.run_once_controlled(
        "if printf corrupted > output/scope/config; then exit 91; fi; printf confined",
        options,
    );
    let control = turn.control();
    let mut turn = Box::pin(turn);

    // Act
    tokio::select! {
        () = super::fixture::wait_file(&ready) => {},
        result = &mut turn => std::panic::resume_unwind(Box::new(format!("victim finished before gate: {result:?}"))),
    }
    let attacker = workspace
        .run("/bin/mv output/scope output/original; /bin/ln -s ../.git output/scope")
        .await;
    assert_eq!(
        attacker.exit_code,
        Some(0),
        "alias positive control: {attacker:?}"
    );
    assert!(root.join("output/scope").is_symlink());
    std::fs::write(&release, "release").expect("release victim after alias replacement");
    let outcome = turn.await.expect("victim");
    let outcome: CommandOutcome = serde_json::from_value(outcome.into_output()).expect("outcome");
    control
        .commands_settled()
        .await
        .expect("best-effort cleanup");

    // Assert
    assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
    assert_eq!(outcome.stdout, "confined");
    assert_eq!(
        outcome.cleanup_scope,
        ag_harness::CommandCleanupScope::ProcessGroupBestEffort
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".git/config")).expect("metadata"),
        "protected"
    );
}
