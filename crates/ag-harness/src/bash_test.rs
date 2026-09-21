use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::command_journal::CommandCleanupScope;
use crate::{
    BashArguments, BashConfig, BashError, BashExecutor, BashProcess, ExecutionError, OutputSchema,
    StoredTurnOptions, Tool, ToolCall, ToolCallArguments, ToolDefinition, ToolPolicy, TurnLimits,
    TurnOptions, UnsandboxedExecutor,
};

fn configuration() -> BashConfig {
    BashConfig::new(
        "/trusted/launcher".into(),
        "/bin/bash".into(),
        "revision-1".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("configuration")
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
    TurnOptions::new(
        OutputSchema::new(json!({"type":"object"})).expect("schema"),
        ToolPolicy::default().allow(Tool::Bash),
        TurnLimits::default(),
    )
    .with_bash(configuration)
}

#[test]
fn validates_arguments_and_exposes_bounded_tool_contract_without_debug_content() {
    // Arrange
    let source = "printf secret-command";
    let definition = ToolDefinition::bash();

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
        .is_err()
    );
    assert_eq!(definition.parameters()["additionalProperties"], false);
}

#[test]
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
        .with_host_information()
        .with_linux_bubblewrap("/usr/bin/bwrap".into())
        .expect("backend");
    let turn_options = options(configured.clone());

    // Act
    let encoded = StoredTurnOptions::encode(&turn_options);
    let decoded = StoredTurnOptions::decode(&encoded).expect("snapshot");

    // Assert
    assert!(!encoded.contains("secret-environment-value"));
    assert!(!format!("{configured:?}").contains("secret-environment-value"));
    assert!(encoded.contains("TOKEN"));
    assert!(encoded.contains("revision-1"));
    assert!(decoded.continuation_compatible(&turn_options));
    assert!(!decoded.continuation_compatible(&options(original)));
    assert_eq!(turn_options.bash(), Some(&configured));
}

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
    let decoded = StoredTurnOptions::decode(&selected_encoded).expect("snapshot");
    assert!(decoded.continuation_compatible(&selected_options));
    assert!(!decoded.continuation_compatible(&native_options));
    assert!(
        StoredTurnOptions::decode(&native_encoded)
            .expect("legacy-shaped snapshot")
            .continuation_compatible(&native_options)
    );
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
    assert!(valid.clone().with_network().is_err());
    assert!(valid.clone().with_read("relative".into()).is_err());
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
        ("ok", Duration::from_secs(1), 8193),
    ] {
        assert!(
            BashConfig::new(
                "/launcher".into(),
                "/bin/bash".into(),
                revision.into(),
                duration,
                budget
            )
            .is_err()
        );
    }
    assert_eq!(
        BashConfig::new(
            "relative-launcher".into(),
            "/bin/bash".into(),
            "ok".into(),
            Duration::from_secs(1),
            1,
        )
        .err(),
        Some(BashError::InvalidPolicy)
    );
}
