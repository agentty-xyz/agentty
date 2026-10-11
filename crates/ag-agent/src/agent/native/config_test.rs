use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use ag_contracts::{PermissionMode, ProviderCallBudget, ReasoningLevel};
use ag_harness::model::{ModelRequest, ReasoningEffort};
use ag_harness::provider::{ModelConfiguration, ModelProvider};
use ag_harness::{Model, TurnInput};
use ag_protocol::ProtocolRequestProfile;
use ag_session::AgentModel;

use super::{
    BudgetedModel, ModelSelection, NativeHarnessConfig, ResumeOnlyModel, bash_environment,
    default_model, output_schema, reasoning_effort, repository, turn_options, unicode_environment,
};

/// Builds an environment snapshot from string pairs.
fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

#[test]
/// Picks the first catalog model whose provider has a nonempty key and,
/// without a provider default, a base URL.
fn default_model_follows_the_first_configured_provider() {
    // Arrange
    let cases = [
        (environment(&[]), None),
        (environment(&[("MODEL_API_KEY", " ")]), None),
        (
            environment(&[("MODEL_API_KEY", "key")]),
            Some(AgentModel::MuseSpark13),
        ),
        (environment(&[("KIMI_API_KEY", "key")]), None),
        (
            environment(&[("KIMI_API_KEY", "key"), ("KIMI_BASE_URL", "http://kimi")]),
            Some(AgentModel::KimiK3),
        ),
        (
            environment(&[
                ("DASHSCOPE_API_KEY", "key"),
                ("DASHSCOPE_BASE_URL", "http://qwen"),
            ]),
            Some(AgentModel::Qwen38Max),
        ),
        (
            environment(&[
                ("DASHSCOPE_API_KEY", "key"),
                ("DASHSCOPE_BASE_URL", "http://qwen"),
                ("MODEL_API_KEY", "key"),
            ]),
            Some(AgentModel::MuseSpark13),
        ),
    ];

    // Act / Assert
    for (environment, expected) in cases {
        assert_eq!(default_model(&environment), expected, "{environment:?}");
    }
}

#[test]
/// Skips variables whose name or value is not valid Unicode instead of
/// panicking.
fn unicode_environment_skips_invalid_variables() {
    // Arrange
    let invalid = || OsString::from_vec(vec![0x66, 0x6f, 0xff]);
    let variables = [
        (OsString::from("MODEL_API_KEY"), OsString::from("key")),
        (OsString::from("LATIN1_VALUE"), invalid()),
        (invalid(), OsString::from("value")),
    ];

    // Act
    let environment = unicode_environment(variables);

    // Assert
    assert_eq!(
        environment,
        BTreeMap::from([("MODEL_API_KEY".to_string(), "key".to_string())])
    );
}

#[tokio::test]
/// Reports the recorded identity without credentials and refuses requests.
async fn resume_only_model_never_sends_requests() {
    // Arrange
    let metadata = ModelConfiguration::new(ModelProvider::Kimi, "kimi-k3")
        .base_url("http://127.0.0.1:9")
        .client_from_environment(|_| Ok("key".to_string()))
        .expect("kimi client")
        .metadata()
        .clone();
    let model = ResumeOnlyModel {
        metadata: metadata.clone(),
    };
    let schema = output_schema(ProtocolRequestProfile::UtilityPrompt).expect("schema");

    // Act
    let error = Model::complete(&model, ModelRequest::new("Hi", schema))
        .await
        .expect_err("resume-only model must not complete");

    // Assert
    assert_eq!(Model::metadata(&model), Some(metadata));
    assert!(error.to_string().contains("can only resume"), "{error}");
}

#[test]
/// Resolves catalog models and rejects mismatched registration keys.
fn model_selection_round_trips_registration_keys() {
    // Arrange
    let selection = ModelSelection::resolve("kimi-k3").expect("kimi-k3 should resolve");

    // Act
    let key = selection.key();

    // Assert
    assert_eq!(key, "kimi/kimi-k3");
    assert_eq!(ModelSelection::from_key(&key), Some(selection));
    assert_eq!(ModelSelection::from_key("kimi-k3"), None);
    assert_eq!(ModelSelection::from_key("qwen/kimi-k3"), None);
    assert!(ModelSelection::resolve("claude-opus-5-5").is_err());
}

#[tokio::test]
/// Places each session database in its own directory and rejects paths.
async fn session_database_uses_one_directory_per_session() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let config = NativeHarnessConfig::new(data_root.path().to_path_buf());

    // Act
    let database = config.session_database("session-1").await;

    // Assert
    assert_eq!(
        database,
        Ok(data_root.path().join("session-1").join("harness.db"))
    );
    assert!(data_root.path().join("session-1").is_dir());
    for invalid in ["", ".", "..", "a/b", "a\\b"] {
        assert!(config.session_database(invalid).await.is_err(), "{invalid}");
    }
    assert!(!config.environment().is_empty());
}

#[tokio::test]
/// Reports a session directory that cannot be created.
async fn session_database_reports_directory_failures() {
    // Arrange
    let data_root = tempfile::NamedTempFile::new().expect("file in place of a directory");
    let config = NativeHarnessConfig::new(data_root.path().to_path_buf());

    // Act
    let database = config.session_database("session-1").await;

    // Assert
    assert!(
        database
            .expect_err("a file root should fail")
            .contains("Failed to create the harness session directory")
    );
}

#[test]
/// Grants essentials first and withholds provider keys and endpoints.
fn bash_environment_orders_essentials_and_drops_provider_variables() {
    // Arrange
    let environment = environment(&[
        ("AAA", "first"),
        ("DASHSCOPE_API_KEY", "secret"),
        ("HOME", "/home/test"),
        ("KIMI_BASE_URL", "http://kimi"),
        ("MODEL_API_KEY", "secret"),
        ("PATH", "/usr/bin"),
    ]);

    // Act
    let granted = bash_environment(&environment)
        .into_iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(granted, ["PATH", "HOME", "AAA"]);
}

#[test]
/// Keeps tool-critical variables within the harness grant limit when the host
/// environment exceeds it.
fn bash_environment_prioritizes_tool_variables_in_large_environments() {
    // Arrange
    let mut environment = (0..100)
        .map(|index| (format!("AAA_{index:03}"), "value".to_string()))
        .collect::<BTreeMap<_, _>>();
    environment.insert("SSH_AUTH_SOCK".to_string(), "/tmp/agent.sock".to_string());
    environment.insert("XDG_CACHE_HOME".to_string(), "/tmp/cache".to_string());

    // Act
    let granted = bash_environment(&environment)
        .into_iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(granted[..2], ["SSH_AUTH_SOCK", "XDG_CACHE_HOME"]);
    assert_eq!(granted.len(), 102);
}

#[test]
/// Skips variables beyond the harness grant limit instead of failing.
fn turn_options_tolerate_oversized_environments() {
    // Arrange
    let environment = (0..100)
        .map(|index| (format!("VAR_{index:03}"), "value".to_string()))
        .collect::<BTreeMap<_, _>>();

    // Act
    let edit = turn_options(
        ProtocolRequestProfile::SessionTurn,
        PermissionMode::AutoEdit,
        &environment,
    );
    let read_only = turn_options(
        ProtocolRequestProfile::SessionTurn,
        PermissionMode::ReadOnly,
        &environment,
    );

    // Assert
    let edit = edit.expect("edit options should build");
    assert!(edit.bash().is_some());
    assert!(edit.tool_policy().allows(ag_harness::Tool::Write));
    let read_only = read_only.expect("read-only options should build");
    assert!(read_only.bash().is_none());
    assert!(!read_only.tool_policy().allows(ag_harness::Tool::Write));
    assert!(output_schema(ProtocolRequestProfile::UtilityPrompt).is_ok());
}

#[test]
/// Finds the first usable absolute Git and reports invalid worktrees.
fn repository_resolves_git_from_path() {
    // Arrange
    let worktree = tempfile::tempdir().expect("worktree");
    let real_path = std::env::var("PATH").expect("test PATH should be set");
    let with_decoys = environment(&[(
        "PATH",
        &format!("relative/bin:/nonexistent-agentty-bin:{real_path}"),
    )]);

    // Act
    let found = repository(worktree.path(), &with_decoys);
    let missing_git = repository(worktree.path(), &environment(&[("PATH", "/nonexistent")]));
    let missing_root = repository(
        &PathBuf::from("/nonexistent-agentty-worktree"),
        &environment(&[("PATH", &real_path)]),
    );

    // Assert
    assert!(found.is_ok(), "{found:?}");
    assert_eq!(
        missing_git.err(),
        Some("The harness needs a Git executable on PATH.".to_string())
    );
    assert!(
        missing_root
            .expect_err("missing worktree should fail")
            .starts_with("Invalid harness worktree")
    );
}

#[test]
/// Maps every reasoning level to the matching harness effort.
fn reasoning_levels_map_one_to_one() {
    // Arrange
    let cases = [
        (ReasoningLevel::Low, ReasoningEffort::Low),
        (ReasoningLevel::Medium, ReasoningEffort::Medium),
        (ReasoningLevel::High, ReasoningEffort::High),
        (ReasoningLevel::XHigh, ReasoningEffort::XHigh),
        (ReasoningLevel::Max, ReasoningEffort::Max),
    ];

    // Act / Assert
    for (level, effort) in cases {
        assert_eq!(reasoning_effort(level), effort);
    }
}

#[tokio::test]
/// Delegates validation and metadata, and stops when the budget is spent.
async fn budgeted_model_charges_before_each_request() {
    // Arrange
    let client = ModelConfiguration::new(ModelProvider::Muse, "muse-spark-1.3")
        .base_url("http://127.0.0.1:9")
        .client_from_environment(|_| Ok("key".to_string()))
        .expect("client should build");
    let model = BudgetedModel {
        budget: ProviderCallBudget::new(0),
        inner: client,
    };
    let schema = output_schema(ProtocolRequestProfile::UtilityPrompt).expect("schema");

    // Act
    let completion = model
        .complete(ModelRequest::new("Hi", schema.clone()))
        .await;

    // Assert
    assert_eq!(
        model
            .metadata()
            .map(|metadata| metadata.model().to_string()),
        Some("muse-spark-1.3".to_string())
    );
    assert!(model.validate_schema(&schema).is_ok());
    assert!(model.validate_input(&TurnInput::from("Hi")).is_ok());
    let error = completion.expect_err("an exhausted budget should refuse the request");
    assert!(
        error.to_string().contains("provider call limit reached"),
        "{error}"
    );
}
