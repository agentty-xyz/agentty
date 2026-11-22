//! Bash tool configuration unit tests for argument validation, grant limits,
//! policy serialization and snapshots, and executor identity.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::bash::{
    BashArguments, BashConfig, BashError, BashExecutor, BashPolicySnapshot, BashProcess,
    Enforcement, ExecutionError, UnsandboxedExecutor,
};
use crate::command_journal::CommandCleanupScope;
use crate::store::StoredTurnOptions;
use crate::tool::{ToolCall, ToolCallArguments, ToolDefinition};
#[expect(deprecated, reason = "covers the legacy policy serialization")]
use crate::{OutputSchema, Tool, ToolPolicy, ToolSet, TurnOptions};

/// Returns the native sandbox configuration, or a host executor's without
/// the `native-sandbox` feature.
fn configuration() -> BashConfig {
    #[cfg(feature = "native-sandbox")]
    let configuration = BashConfig::new(
        "/trusted/launcher".into(),
        "/bin/bash".into(),
        "revision-1".into(),
        Duration::from_secs(5),
        1024,
    );
    #[cfg(not(feature = "native-sandbox"))]
    let configuration = executor_configuration(Arc::new(NamedExecutor("container".to_string())));

    configuration.expect("configuration")
}

fn executor_configuration(executor: Arc<dyn BashExecutor>) -> Result<BashConfig, BashError> {
    BashConfig::for_executor(
        executor,
        "/bin/bash".into(),
        "revision-1".into(),
        Duration::from_secs(5),
        1024,
    )
}

struct NamedExecutor(String);

impl BashExecutor for NamedExecutor {
    fn identity(&self) -> &str {
        &self.0
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::FileSystem
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

/// A host executor that enforces both the filesystem policy and a network
/// denial.
struct IsolatingExecutor(String);

impl BashExecutor for IsolatingExecutor {
    fn identity(&self) -> &str {
        &self.0
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::FileSystemAndNetwork
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

#[test]
fn grant_limits_reject_the_next_entry_without_changing_the_original() {
    // Arrange
    let mut policy = configuration();
    for index in 0..64 {
        policy = policy
            .with_read(format!("/runtime/{index}").into())
            .expect("read within limit")
            .with_write(format!("output/{index}").into())
            .expect("write within limit");
    }

    // Act / Assert
    assert_eq!(
        policy.clone().with_read("/extra".into()),
        Err(BashError::InvalidPolicy)
    );
    assert_eq!(
        policy.clone().with_write("extra".into()),
        Err(BashError::InvalidPolicy)
    );
    assert_eq!(policy.snapshot.external_reads.len(), 64);
    assert_eq!(policy.snapshot.workspace_writes.len(), 64);
}

fn options(configuration: BashConfig) -> TurnOptions {
    TurnOptions::from_output(OutputSchema::new(json!({"type":"object"})).expect("schema"))
        .with_tools(ToolSet::empty().with_bash())
        .with_bash(configuration)
}

#[test]
fn validates_arguments_and_exposes_bounded_tool_contract_without_debug_content() {
    // Arrange
    let source = "printf secret-command";
    let definition = ToolDefinition::bash(Enforcement::FileSystemAndNetwork);

    // Act
    let arguments = BashArguments::new(source.into()).expect("arguments");
    let call = ToolCall::from_json(
        "call".into(),
        "bash",
        &json!({"command": source}).to_string(),
        None,
    )
    .expect("call");

    // Assert
    assert_eq!(arguments.command(), source);
    assert!(!format!("{arguments:?}{call:?}").contains("secret-command"));
    assert!(matches!(call.arguments(), ToolCallArguments::Bash(_)));
    assert_eq!(call.name(), definition.name());
    assert!(call.read_arguments().is_none());
    assert!(call.write_arguments().is_none());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&call.arguments_json().expect("arguments"))
            .expect("JSON"),
        json!({"command":source})
    );
    for invalid in [
        String::new(),
        " \n".to_string(),
        "a\0b".to_string(),
        "x".repeat(65537),
    ] {
        assert_eq!(
            BashArguments::new(invalid),
            Err(BashError::InvalidArguments)
        );
    }
    assert!(
        ToolCall::from_json(
            "call".into(),
            "bash",
            r#"{"command":"true","cwd":"/"}"#,
            None
        )
        .expect("kept as malformed")
        .malformed_arguments()
        .is_some()
    );
    assert_eq!(definition.parameters()["additionalProperties"], false);
}

#[test]
#[expect(deprecated, reason = "covers the legacy policy serialization")]
fn default_denial_and_legacy_policy_serialization_remain_stable() {
    // Arrange
    let policy = ToolPolicy::default();

    // Act
    let allowed = policy.allow(Tool::Bash);
    let denied = allowed.deny(Tool::Bash);

    // Assert
    assert!(!policy.allows(Tool::Bash));
    assert!(allowed.allows(Tool::Bash));
    assert_eq!(denied, policy);
    assert_eq!(
        serde_json::to_value(policy).expect("policy"),
        json!({"read":false,"write":false})
    );
    assert!(
        serde_json::from_value::<ToolPolicy>(json!({"read":true,"write":false}))
            .expect("legacy")
            .allows(Tool::Read)
    );
}

#[test]
fn policy_is_immutable_and_snapshots_identify_environment_without_values() {
    // Arrange
    let original = configuration();
    let configured = original
        .clone()
        .with_read("/bin".into())
        .expect("read")
        .with_write("output".into())
        .expect("write")
        .with_environment("TOKEN".into(), "secret-environment-value".into())
        .expect("environment")
        .with_host_information();
    #[cfg(feature = "native-sandbox")]
    let configured = configured
        .with_linux_bubblewrap("/usr/bin/bwrap".into())
        .expect("backend");
    let turn_options = options(configured.clone());

    // Act
    let encoded = StoredTurnOptions::encode(&turn_options);
    let decoded = StoredTurnOptions::decode(&encoded);

    // Assert
    assert!(!encoded.contains("secret-environment-value"));
    assert!(!format!("{configured:?}").contains("secret-environment-value"));
    assert!(encoded.contains("TOKEN"));
    assert!(encoded.contains("revision-1"));
    assert!(decoded.is_ok());
    assert_ne!(encoded, StoredTurnOptions::encode(&options(original)));
    assert_eq!(turn_options.bash(), Some(&configured));
}

#[cfg(feature = "native-sandbox")]
#[test]
fn executor_selection_records_identity_and_stays_distinct_from_native_snapshots() {
    // Arrange
    let native = configuration();
    let selected = executor_configuration(Arc::new(UnsandboxedExecutor::without_isolation()))
        .expect("executor configuration");
    let equivalent = executor_configuration(Arc::new(UnsandboxedExecutor::without_isolation()))
        .expect("executor configuration");
    let native_options = options(native.clone());
    let selected_options = options(selected.clone());

    // Act
    let native_encoded = StoredTurnOptions::encode(&native_options);
    let selected_encoded = StoredTurnOptions::encode(&selected_options);

    // Assert
    assert_eq!(
        selected, equivalent,
        "identity, not the instance, compares executors"
    );
    assert_ne!(native, selected);
    assert_eq!(selected.snapshot.executor, "unsandboxed");
    assert_eq!(selected.snapshot.launcher, None);
    assert!(
        !native_encoded.contains("executor"),
        "native snapshots keep their pre-executor encoding: {native_encoded}"
    );
    assert!(selected_encoded.contains("unsandboxed"));
    assert_ne!(selected_encoded, native_encoded);
    assert!(StoredTurnOptions::decode(&selected_encoded).is_ok());
    assert!(StoredTurnOptions::decode(&native_encoded).is_ok());
}

#[test]
fn configuration_reports_the_selected_executor_enforcement() {
    // Arrange
    let custom = executor_configuration(Arc::new(NamedExecutor("container".to_string())))
        .expect("executor configuration");
    let unsandboxed = executor_configuration(Arc::new(UnsandboxedExecutor::without_isolation()))
        .expect("executor configuration");

    // Act
    let enforcements = [custom.enforcement(), unsandboxed.enforcement()];

    // Assert
    assert_eq!(enforcements, [Enforcement::FileSystem, Enforcement::None]);
    #[cfg(feature = "native-sandbox")]
    assert_eq!(
        configuration().enforcement(),
        Enforcement::FileSystemAndNetwork
    );
}

#[test]
fn legacy_snapshots_without_an_executor_decode_as_the_native_default() {
    // Arrange
    let legacy = json!({
        "bash": "/bin/bash",
        "capture_bytes": 1024,
        "environment_names": [],
        "external_reads": [],
        "host_information": false,
        "launcher": "/trusted/launcher",
        "linux_bubblewrap": null,
        "revision": "revision-1",
        "timeout": {"secs": 5, "nanos": 0},
        "workspace_writes": []
    });

    // Act
    let snapshot: BashPolicySnapshot =
        serde_json::from_value(legacy.clone()).expect("legacy snapshot");

    // Assert
    assert_eq!(snapshot.executor, "native");
    assert_eq!(serde_json::to_value(&snapshot).expect("snapshot"), legacy);
}

#[test]
fn executor_identities_are_validated_and_cannot_claim_the_native_default() {
    // Arrange
    let long = "x".repeat(257);
    let invalid = ["", " \n", &long, "nul\0id", "native"];

    // Act / Assert
    for identity in invalid {
        assert_eq!(
            executor_configuration(Arc::new(NamedExecutor(identity.to_string()))).err(),
            Some(BashError::InvalidPolicy)
        );
    }
    assert!(executor_configuration(Arc::new(NamedExecutor("container".to_string()))).is_ok());
    assert_eq!(
        BashConfig::for_executor(
            Arc::new(NamedExecutor("container".to_string())),
            "relative-bash".into(),
            "revision".into(),
            Duration::from_secs(5),
            1024,
        )
        .err(),
        Some(BashError::InvalidPolicy)
    );
}

#[test]
fn malformed_or_unsupported_host_grants_are_rejected() {
    // Arrange
    let valid = configuration();

    // Act / Assert
    assert!(valid.clone().with_read("relative".into()).is_err());
    #[cfg(feature = "native-sandbox")]
    assert!(
        valid
            .clone()
            .with_linux_bubblewrap("relative".into())
            .is_err()
    );
    for path in ["/absolute", "../escape", "output/.Git/data"] {
        assert!(valid.clone().with_write(path.into()).is_err());
    }
    for name in ["", "a=b", "a\0b"] {
        assert!(
            valid
                .clone()
                .with_environment(name.into(), "value".into())
                .is_err()
        );
    }
    assert!(
        valid
            .clone()
            .with_environment("KEY".into(), "nul\0".into())
            .is_err()
    );
    assert!(
        valid
            .with_environment("KEY".into(), "one".into())
            .expect("grant")
            .with_environment("KEY".into(), "two".into())
            .is_err()
    );
    #[cfg(feature = "native-sandbox")]
    assert_eq!(
        BashConfig::new(
            "relative-launcher".into(),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(5),
            1024,
        )
        .err(),
        Some(BashError::InvalidPolicy)
    );
    for (revision, duration, budget) in [
        ("", Duration::from_secs(1), 1),
        ("ok", Duration::ZERO, 1),
        ("ok", Duration::from_secs(3601), 1),
        ("ok", Duration::from_secs(1), 0),
        (
            "ok",
            Duration::from_secs(1),
            BashConfig::MAX_CAPTURE_BYTES + 1,
        ),
    ] {
        assert!(
            BashConfig::for_executor(
                Arc::new(NamedExecutor("container".to_string())),
                "/bin/bash".into(),
                revision.into(),
                duration,
                budget
            )
            .is_err()
        );
    }
}

#[test]
fn development_grants_validate_and_record_only_what_they_set() {
    // Arrange
    let valid = configuration();
    let legacy = valid.fingerprint();

    // Act
    let granted = valid
        .clone()
        .with_system_reads()
        .with_git_writes()
        .with_network()
        .with_temporary_directory("/private/tmp/session".into())
        .expect("temporary directory");

    // Assert
    for name in ["capabilities", "temporary_directory"] {
        assert!(legacy.get(name).is_none(), "{name}");
    }
    assert_eq!(
        granted.fingerprint()["capabilities"],
        json!(["git_writes", "network", "system_reads"])
    );
    assert_eq!(
        granted.fingerprint()["temporary_directory"],
        "/private/tmp/session"
    );
    let decoded: BashPolicySnapshot =
        serde_json::from_value(granted.fingerprint()).expect("snapshot");
    assert_eq!(decoded, granted.snapshot);
    let legacy: BashPolicySnapshot = serde_json::from_value(legacy).expect("legacy snapshot");
    assert_eq!(legacy, valid.snapshot);
    assert!(
        granted
            .clone()
            .with_temporary_directory("/private/tmp/other".into())
            .is_err()
    );
    assert!(
        valid
            .clone()
            .with_temporary_directory("relative".into())
            .is_err()
    );
    assert!(
        granted
            .with_environment("TMPDIR".into(), "/elsewhere".into())
            .is_err()
    );
    assert!(
        valid
            .with_environment("TMPDIR".into(), "/elsewhere".into())
            .expect("host temporary directory")
            .with_temporary_directory("/private/tmp/session".into())
            .is_err()
    );
}

/// A host executor that enforces the filesystem policy, hidden paths
/// included.
struct HidingExecutor;

impl BashExecutor for HidingExecutor {
    fn identity(&self) -> &'static str {
        "hiding"
    }

    fn enforcement(&self) -> Enforcement {
        Enforcement::FileSystem
    }

    fn supports_hidden_paths(&self) -> bool {
        true
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

/// Returns a configuration of an executor that enforces hidden paths.
fn hiding_configuration() -> BashConfig {
    executor_configuration(Arc::new(HidingExecutor)).expect("configuration")
}

/// Binds a Unix-domain socket named `name` in `directory`, a short path:
/// socket paths are limited to about 100 bytes.
fn socket(directory: &std::path::Path, name: &str) -> (std::path::PathBuf, UnixListener) {
    let path = directory.join(name);
    let listener = UnixListener::bind(&path).expect("socket");

    (path, listener)
}

/// Writes an executable script named `name` in `directory`.
fn program(directory: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    std::fs::write(&path, "#!/bin/sh\n").expect("program");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");

    path
}

#[test]
fn coding_grants_validate_and_record_only_what_they_set() {
    // Arrange
    let directory = tempfile::tempdir_in("/tmp").expect("fixture directory");
    let root = directory.path().canonicalize().expect("canonical fixture");
    let (agent, _listener) = socket(&root, "agent");
    std::os::unix::fs::symlink(&agent, root.join("alias")).expect("socket alias");
    let docker = program(&root, "docker");
    std::fs::write(root.join("plain"), "").expect("non-executable file");
    let valid = hiding_configuration();
    let legacy = valid.fingerprint();

    // Act
    let granted = valid
        .clone()
        .with_full_disk_read()
        .expect("full-disk read")
        .with_hidden_path("/home/.env".into())
        .expect("hidden path")
        .with_writable_root("/home/.cargo/registry".into())
        .expect("writable root")
        .with_unix_socket(root.join("alias"))
        .expect("socket")
        .with_excluded_command(docker.clone())
        .expect("excluded command");

    // Assert
    for name in [
        "capabilities",
        "excluded_commands",
        "hidden_paths",
        "unix_sockets",
        "writable_roots",
    ] {
        assert!(legacy.get(name).is_none(), "{name}");
    }
    let fingerprint = granted.fingerprint();
    assert_eq!(fingerprint["capabilities"], json!(["full_disk_read"]));
    assert_eq!(fingerprint["excluded_commands"], json!({"docker": docker}));
    assert_eq!(fingerprint["hidden_paths"], json!(["/home/.env"]));
    // The socket grant is pinned to the socket the link named.
    assert_eq!(fingerprint["unix_sockets"], json!([agent]));
    assert_eq!(
        fingerprint["writable_roots"],
        json!(["/home/.cargo/registry"])
    );
    let decoded: BashPolicySnapshot = serde_json::from_value(fingerprint).expect("snapshot");
    assert_eq!(decoded, granted.snapshot);
    for rejected in [
        valid.clone().with_hidden_path("relative".into()),
        valid.clone().with_hidden_path("/".into()),
        granted.clone().with_hidden_path("/home/.env".into()),
        valid.clone().with_unix_socket("relative".into()),
        valid.clone().with_unix_socket(root.join("missing")),
        valid.clone().with_unix_socket(root.join("plain")),
        granted.clone().with_unix_socket(agent),
        valid.clone().with_writable_root("relative".into()),
        valid.clone().with_writable_root("/".into()),
        valid.clone().with_writable_root("/repo/.git".into()),
        granted
            .clone()
            .with_writable_root("/home/.cargo/registry".into()),
        valid.clone().with_excluded_command("docker".into()),
        valid.clone().with_excluded_command(root.join("missing")),
        valid.clone().with_excluded_command(root.join("plain")),
        valid.clone().with_excluded_command(root.clone()),
        valid.clone().with_excluded_command(program(&root, "-rf")),
        valid.clone().with_excluded_command(program(&root, "a b")),
        valid.with_excluded_command(program(&root, &"x".repeat(65))),
        granted.with_excluded_command(docker),
    ] {
        assert_eq!(rejected, Err(BashError::InvalidPolicy));
    }
}

#[test]
fn excluded_environment_values_are_validated_and_recorded_by_name() {
    // Arrange
    let valid = hiding_configuration();

    // Act
    let granted = valid
        .clone()
        .with_excluded_environment("GH_TOKEN".into(), "token".into())
        .expect("excluded value");

    // Assert
    assert!(
        valid
            .fingerprint()
            .get("excluded_environment_names")
            .is_none()
    );
    assert_eq!(
        granted.fingerprint()["excluded_environment_names"],
        json!(["GH_TOKEN"])
    );
    assert!(!granted.fingerprint().to_string().contains("token\""));
    assert_ne!(granted, valid);
    for rejected in [
        valid
            .clone()
            .with_excluded_environment("PATH".into(), "/bin".into()),
        valid
            .clone()
            .with_excluded_environment(String::new(), "x".into()),
        valid
            .clone()
            .with_excluded_environment("A=B".into(), "x".into()),
        valid.with_excluded_environment("NAME".into(), "a\0b".into()),
        granted.with_excluded_environment("GH_TOKEN".into(), "again".into()),
    ] {
        assert_eq!(rejected, Err(BashError::InvalidPolicy));
    }
}

#[test]
fn hidden_paths_require_an_executor_that_enforces_them() {
    // Arrange
    let ignoring =
        executor_configuration(Arc::new(NamedExecutor("container".into()))).expect("configuration");
    let unsandboxed = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("unsandboxed configuration");

    // Act / Assert
    assert_eq!(
        ignoring.clone().with_full_disk_read(),
        Err(BashError::InvalidPolicy)
    );
    assert_eq!(
        ignoring.with_hidden_path("/home/.env".into()),
        Err(BashError::InvalidPolicy)
    );
    // An executor that enforces nothing hides nothing anyway.
    assert!(unsandboxed.clone().with_full_disk_read().is_ok());
    assert!(unsandboxed.with_hidden_path("/home/.env".into()).is_ok());
}

#[test]
fn coding_grant_limits_reject_the_next_entry() {
    // Arrange
    let directory = tempfile::tempdir_in("/tmp").expect("fixture directory");
    let root = directory.path().canonicalize().expect("canonical fixture");
    let mut listeners = Vec::new();
    let mut policy = hiding_configuration();
    for index in 0..64 {
        let (path, listener) = socket(&root, &format!("s{index}"));
        listeners.push(listener);
        policy = policy
            .with_hidden_path(format!("/hidden/{index}").into())
            .expect("hidden path within limit")
            .with_writable_root(format!("/cache/{index}").into())
            .expect("root within limit")
            .with_unix_socket(path)
            .expect("socket within limit")
            .with_excluded_command(program(&root, &format!("tool-{index}")))
            .expect("command within limit");
    }
    let (extra, _listener) = socket(&root, "extra");

    // Act / Assert
    for rejected in [
        policy.clone().with_hidden_path("/extra".into()),
        policy.clone().with_writable_root("/extra".into()),
        policy.clone().with_unix_socket(extra),
        policy.with_excluded_command(program(&root, "extra-tool")),
    ] {
        assert_eq!(rejected, Err(BashError::InvalidPolicy));
    }
}

#[test]
fn only_simple_commands_of_excluded_programs_leave_the_sandbox() {
    // Arrange
    let directory = tempfile::tempdir().expect("fixture directory");
    let root = directory.path().canonicalize().expect("canonical fixture");
    let docker = program(&root, "docker");
    let configuration = hiding_configuration()
        .with_excluded_command(docker.clone())
        .expect("excluded command");

    // Act / Assert
    for (command, words) in [
        ("docker ps", vec!["ps"]),
        (
            "  docker\tbuild  -t image:latest --label=a=b .",
            vec!["build", "-t", "image:latest", "--label=a=b", "."],
        ),
        ("docker", vec![]),
    ] {
        assert_eq!(
            configuration.excluded_program(command),
            Some((
                docker.as_path(),
                words.into_iter().map(str::to_owned).collect()
            )),
            "{command}"
        );
    }
    // Anything a shell would expand, quote, or interpret stays sandboxed,
    // so workspace file names can never become arguments.
    for command in [
        "dockerd ps",
        "/usr/bin/docker ps",
        "./docker ps",
        "FOO=1 docker ps",
        "docker ps; rm -rf x",
        "docker ps && true",
        "docker ps | cat",
        "docker ps > out",
        "docker run $(id)",
        "docker run `id`",
        "docker run \\\n x",
        "docker ps\nrm x",
        "docker (ps)",
        "docker fetch *",
        "docker run ?",
        "docker run [ab]",
        "docker run {a,b}",
        "docker run ~/x",
        "docker run !1",
        "docker run # x",
        "docker run 'image name'",
        "docker run \"image\"",
        "docker run\u{7}x",
        // Characters that display differently from the words they form.
        "docker run\u{a0}x",
        "docker run\u{3000}x",
        "docker run\u{200b}x",
        "docker run\u{feff}x",
        "docker run\u{202e}x",
        "docker run\u{2066}x",
        "docker run\u{2028}x",
        "docker run\u{2029}x",
        "docker run caf\u{e9}",
        "",
        "git status",
    ] {
        assert_eq!(configuration.excluded_program(command), None, "{command}");
    }
}

#[test]
fn network_grant_never_overstates_the_enforced_isolation() {
    // Arrange
    let unenforced = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("unsandboxed configuration");
    let filesystem = executor_configuration(Arc::new(NamedExecutor("container".to_string())))
        .expect("filesystem configuration");
    let isolating = executor_configuration(Arc::new(IsolatingExecutor("isolating".to_string())))
        .expect("isolating configuration");

    // Act / Assert
    assert_eq!(isolating.enforcement(), Enforcement::FileSystemAndNetwork);
    assert_eq!(
        isolating.with_network().enforcement(),
        Enforcement::FileSystem
    );
    #[cfg(feature = "native-sandbox")]
    assert_eq!(
        configuration().with_network().enforcement(),
        Enforcement::FileSystem
    );
    assert_eq!(
        filesystem.with_network().enforcement(),
        Enforcement::FileSystem
    );
    assert_eq!(unenforced.with_network().enforcement(), Enforcement::None);
}

#[test]
fn capture_budget_accepts_the_default_and_the_maximum() {
    // Arrange
    let budgets = [
        BashConfig::DEFAULT_CAPTURE_BYTES,
        BashConfig::MAX_CAPTURE_BYTES,
    ];

    // Act
    let configurations = budgets.map(|budget| {
        BashConfig::for_executor(
            Arc::new(NamedExecutor("container".to_string())),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(1),
            budget,
        )
    });

    // Assert
    for (configuration, budget) in configurations.into_iter().zip(budgets) {
        assert_eq!(
            configuration
                .expect("budget within bounds")
                .snapshot
                .capture_bytes,
            budget
        );
    }
}
