use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use ag_contracts::{PermissionMode, ProviderCallBudget, ReasoningLevel};
use ag_harness::model::{ModelProfile, ModelRequest, ReasoningEffort};
use ag_harness::provider::{ModelConfiguration, ModelProvider};
use ag_harness::{Model, TurnInput};
use ag_protocol::ProtocolRequestProfile;
use ag_session::{AgentModel, HarnessModelStatus, ModelBilling, ServedModel};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    BudgetedModel, FALLBACK_MODEL_PROFILE, ModelSelection, NativeHarnessConfig, ResumeOnlyModel,
    bash_environment, harness_models, output_schema, provider_display_name, reasoning_effort,
    repository, running_context_window, turn_options, unicode_environment,
};
use crate::agent::model_profile;

/// Builds an environment snapshot from string pairs.
fn environment(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// Returns the served model `name`.
fn served(name: &str) -> AgentModel {
    AgentModel::OpenAiCompatible(ServedModel::new(name).expect("served name"))
}

/// Returns a base URL on a port nothing listens on.
fn unreachable_base_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let address = listener.local_addr().expect("address");

    format!("http://{address}/v1")
}

/// Mounts Ollama's `/api/ps` reporting `qwen3:latest` loaded with `window`.
async fn mount_loaded_window(server: &MockServer, window: u64) {
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "qwen3:latest", "context_length": window}]
        })))
        .mount(server)
        .await;
}

#[tokio::test]
/// Lists the configured local model first, then every paid catalog model with
/// the variable its provider still needs.
async fn harness_models_list_local_then_paid_models_with_their_status() {
    // Arrange
    let environment = environment(&[
        ("KIMI_API_KEY", "key"),
        ("MODEL_API_KEY", "key"),
        ("OPENAI_COMPATIBLE_BASE_URL", &unreachable_base_url()),
        ("OPENAI_COMPATIBLE_MODEL", " qwen3:latest "),
    ]);

    // Act
    let options = harness_models(&environment).await;

    // Assert
    let summary = options
        .iter()
        .map(|option| (option.model, option.billing, option.status))
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        vec![
            (
                served("qwen3:latest"),
                ModelBilling::Local,
                HarnessModelStatus::Ready
            ),
            (
                AgentModel::MuseSpark13,
                ModelBilling::Paid,
                HarnessModelStatus::Ready
            ),
            (
                AgentModel::KimiK3,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("KIMI_BASE_URL")
            ),
            (
                AgentModel::KimiK26,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("KIMI_BASE_URL")
            ),
            (
                AgentModel::Qwen38Max,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("DASHSCOPE_API_KEY")
            ),
            (
                AgentModel::QwenPlus,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("DASHSCOPE_API_KEY")
            ),
            (
                AgentModel::Gpt61Sol,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("OPENAI_API_KEY")
            ),
            (
                AgentModel::Gpt6Astra,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("OPENAI_API_KEY")
            ),
            (
                AgentModel::Gpt6Luna,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("OPENAI_API_KEY")
            ),
            (
                AgentModel::Gpt56Terra,
                ModelBilling::Paid,
                HarnessModelStatus::MissingConfiguration("OPENAI_API_KEY")
            ),
        ]
    );
    assert_eq!(options[0].provider, "OpenAI-compatible server");
    assert_eq!(options[1].provider, "Muse");
    assert_eq!(options[2].provider, "Kimi");
    assert_eq!(options[4].provider, "Qwen");
    assert_eq!(options[6].provider, "OpenAI");
    assert_eq!(
        options[1].context_window,
        model_profile::declared(AgentModel::MuseSpark13).map(ModelProfile::context_window_tokens)
    );
    // Shared Codex models show the window the harness runs them with.
    assert_eq!(
        options[6].context_window,
        ModelProvider::OpenAi
            .declared_profile("gpt-6.1-sol", |_| Err(std::env::VarError::NotPresent))
            .expect("OpenAI profiles read no environment")
            .map(ModelProfile::context_window_tokens)
    );
}

#[test]
/// Names every provider the way users know it.
fn providers_have_display_names() {
    // Arrange, Act, Assert
    assert_eq!(
        ModelProvider::all()
            .iter()
            .map(|provider| provider_display_name(*provider))
            .collect::<Vec<_>>(),
        ["Muse", "Kimi", "Qwen", "OpenAI-compatible server"]
    );
}

#[tokio::test]
/// Lists every model a reachable Ollama serves once, marking models without
/// tool calls, and keeps its reported windows.
async fn harness_models_list_discovered_ollama_models() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
            {
                "name": "qwen3:latest",
                "details": {"context_length": 40960},
                "capabilities": ["completion", "tools"]
            },
            {
                "name": "gemma:latest",
                "details": {"context_length": 8192},
                "capabilities": ["completion"]
            }
        ]})))
        .mount(&server)
        .await;
    mount_loaded_window(&server, 4096).await;
    let environment = environment(&[
        (
            "OPENAI_COMPATIBLE_BASE_URL",
            &format!("{}/v1", server.uri()),
        ),
        ("OPENAI_COMPATIBLE_MODEL", "qwen3:latest"),
    ]);

    // Act
    let options = harness_models(&environment).await;

    // Assert
    let local = options
        .iter()
        .filter(|option| option.billing == ModelBilling::Local)
        .collect::<Vec<_>>();
    assert_eq!(local.len(), 2);
    assert_eq!(local[0].model, served("qwen3:latest"));
    assert_eq!(local[0].provider, "Ollama");
    assert_eq!(local[0].status, HarnessModelStatus::Ready);
    assert_eq!(local[0].context_window, Some(40_960));
    assert_eq!(local[0].loaded_context_window, Some(4096));
    assert_eq!(local[1].model, served("gemma:latest"));
    assert_eq!(local[1].status, HarnessModelStatus::NoToolSupport);
}

#[tokio::test]
/// Sizes a local turn to the window the server runs and refuses a window
/// that would silently drop conversation history.
async fn running_context_window_checks_the_local_server() {
    // Arrange
    let roomy = MockServer::start().await;
    mount_loaded_window(&roomy, 32_768).await;
    let cramped = MockServer::start().await;
    mount_loaded_window(&cramped, 4096).await;
    let base_url = |server: &MockServer| {
        environment(&[(
            "OPENAI_COMPATIBLE_BASE_URL",
            &format!("{}/v1", server.uri()),
        )])
    };
    let local = ModelSelection::resolve("openai-compatible/qwen3:latest", &environment(&[]))
        .expect("served model");
    let hosted = ModelSelection::resolve("kimi-k3", &environment(&[])).expect("kimi-k3");
    let unreachable = environment(&[("OPENAI_COMPATIBLE_BASE_URL", &unreachable_base_url())]);

    // Act
    let roomy_window = running_context_window(&local, &base_url(&roomy)).await;
    let cramped_window = running_context_window(&local, &base_url(&cramped)).await;
    let hosted_window = running_context_window(&hosted, &environment(&[])).await;
    let unreachable_window = running_context_window(&local, &unreachable).await;

    // Assert
    assert_eq!(roomy_window, Ok(Some(32_768)));
    let error = cramped_window.expect_err("a 4,096-token window is too small");
    assert!(error.contains("4096-token context window"), "{error}");
    assert!(error.contains("OLLAMA_CONTEXT_LENGTH=32768"), "{error}");
    assert_eq!(hosted_window, Ok(None));
    assert_eq!(unreachable_window, Ok(None));
}

#[test]
/// Registers a served model with the window its server runs.
fn openai_compatible_registration_uses_the_running_window() {
    // Arrange
    let selection = ModelSelection::resolve("openai-compatible/qwen3:latest", &environment(&[]))
        .expect("served model");

    // Act
    let registry = selection
        .registry(&environment(&[]), None, Some(20_000))
        .expect("registry");

    // Assert
    let registration = registry.resolve(&selection.key()).expect("registration");
    assert_eq!(
        registration.capabilities().profile.context_window_tokens(),
        20_000
    );
}

#[test]
/// Resolves `openai-compatible` to the served model the environment names
/// and recorded keys for any served model.
fn openai_compatible_selection_names_the_served_model() {
    // Arrange
    let configured = environment(&[("OPENAI_COMPATIBLE_MODEL", " qwen3:latest ")]);

    // Act
    let selection = ModelSelection::resolve("openai-compatible", &configured)
        .expect("a named served model should resolve");
    let chosen = ModelSelection::resolve("openai-compatible/llama3:8b", &configured)
        .expect("a chosen served model should resolve");
    let missing = ModelSelection::resolve("openai-compatible", &environment(&[]))
        .expect_err("an unnamed served model should fail");

    // Assert
    assert_eq!(selection.key(), "openai-compatible/qwen3:latest");
    assert_eq!(chosen.key(), "openai-compatible/llama3:8b");
    assert_eq!(
        ModelSelection::from_key("openai-compatible/llama3.3:70b").map(|other| other.key()),
        Some("openai-compatible/llama3.3:70b".to_string())
    );
    assert_eq!(ModelSelection::from_key("openai-compatible/ "), None);
    assert!(missing.contains("OPENAI_COMPATIBLE_MODEL"), "{missing}");
    assert!(missing.contains("before starting Agentty"), "{missing}");
}

#[test]
/// Registers the OpenAI-compatible model with the profile the environment
/// declares, without an API key, and reports invalid declarations.
fn openai_compatible_registration_uses_the_declared_profile() {
    // Arrange
    let configured = environment(&[
        ("OPENAI_COMPATIBLE_MODEL", "qwen3:latest"),
        ("OPENAI_COMPATIBLE_CONTEXT_WINDOW", "40960"),
    ]);
    let invalid_window = environment(&[
        ("OPENAI_COMPATIBLE_MODEL", "qwen3:latest"),
        ("OPENAI_COMPATIBLE_CONTEXT_WINDOW", "lots"),
    ]);
    let selection =
        ModelSelection::resolve("openai-compatible", &configured).expect("served model");

    // Act
    let registry = selection
        .registry(&configured, None, None)
        .expect("registry");
    let invalid = selection
        .registry(&invalid_window, None, None)
        .err()
        .expect("an invalid window should fail");

    // Assert
    let registration = registry.resolve(&selection.key()).expect("registration");
    assert_eq!(
        registration.capabilities().profile.context_window_tokens(),
        40_960
    );
    assert_eq!(
        registration.metadata().map(|metadata| metadata.provider()),
        Some("openai_compatible")
    );
    assert!(
        invalid.starts_with("Invalid harness model profile"),
        "{invalid}"
    );
}

#[test]
/// Names the model and endpoint variables, not an optional key, when a
/// keyless provider cannot be configured.
fn openai_compatible_configuration_failures_name_the_model_and_endpoint() {
    // Arrange
    let blank = ModelSelection {
        model: " ".to_string(),
        provider: ModelProvider::OpenAiCompatible,
    };

    // Act
    let error = blank
        .registry(&environment(&[]), None, None)
        .err()
        .expect("a blank served model should fail");

    // Assert
    assert!(
        error.contains("Check OPENAI_COMPATIBLE_MODEL and OPENAI_COMPATIBLE_BASE_URL"),
        "{error}"
    );
    assert!(!error.contains("OPENAI_COMPATIBLE_API_KEY"), "{error}");
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
    let selection =
        ModelSelection::resolve("kimi-k3", &environment(&[])).expect("kimi-k3 should resolve");

    // Act
    let key = selection.key();

    // Assert
    assert_eq!(key, "kimi/kimi-k3");
    assert_eq!(ModelSelection::from_key(&key), Some(selection));
    assert_eq!(ModelSelection::from_key("kimi-k3"), None);
    assert_eq!(ModelSelection::from_key("qwen/kimi-k3"), None);
    assert!(ModelSelection::resolve("claude-opus-5-5", &environment(&[])).is_err());
}

#[test]
/// Registers each served model under the current revision with its declared
/// context profile: the provider catalog's for `OpenAI` models, Agentty's for
/// the others, and the conservative fallback for uncataloged models.
fn registrations_declare_revision_and_per_model_context_profiles() {
    // Arrange
    let environment = environment(&[
        ("DASHSCOPE_API_KEY", "key"),
        ("DASHSCOPE_BASE_URL", "http://qwen"),
        ("KIMI_API_KEY", "key"),
        ("KIMI_BASE_URL", "http://kimi"),
        ("MODEL_API_KEY", "key"),
        ("OPENAI_API_KEY", "sk-key"),
    ]);
    let models = ModelProvider::all().iter().flat_map(|provider| {
        provider
            .known_models()
            .iter()
            .map(move |model| (*provider, *model))
    });

    // Act / Assert
    for (provider, model) in models {
        let selection = ModelSelection::resolve(model, &environment).expect("served model");
        let registry = selection
            .registry(&environment, None, None)
            .expect("registry");
        let registration = registry.resolve(&selection.key()).expect("registration");
        let expected = if provider == ModelProvider::OpenAi {
            provider
                .declared_profile(model, |_| Err(std::env::VarError::NotPresent))
                .expect("OpenAI profiles read no environment")
        } else {
            AgentModel::parse_persisted(model)
                .ok()
                .and_then(model_profile::declared)
        }
        .unwrap_or(FALLBACK_MODEL_PROFILE);
        assert_eq!(registration.identity().revision(), "agentty-native-2");
        assert_eq!(registration.capabilities().profile, expected, "{model}");
    }
    let sol = ModelSelection::resolve("gpt-6.1-sol", &environment).expect("served model");
    let sol_registry = sol.registry(&environment, None, None).expect("registry");
    let sol_profile = sol_registry
        .resolve("openai/gpt-6.1-sol")
        .expect("registration")
        .capabilities()
        .profile;
    assert_ne!(
        Some(sol_profile),
        model_profile::declared(AgentModel::Gpt61Sol),
        "the harness must not reuse the Codex app-server compaction profile"
    );
    assert_eq!(FALLBACK_MODEL_PROFILE.context_window_tokens(), 128_000);
    assert_eq!(FALLBACK_MODEL_PROFILE.reserved_tokens(), 49_673);
    assert_eq!(FALLBACK_MODEL_PROFILE.input_token_budget(), 78_327);
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
        ("OPENAI_API_KEY", "secret"),
        ("OPENAI_BASE_URL", "https://proxy.example/v1"),
        ("OPENAI_COMPATIBLE_API_KEY", "secret"),
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
    assert!(!model.keeps_prefix_across_schemas());
    let error = completion.expect_err("an exhausted budget should refuse the request");
    assert!(
        error.to_string().contains("provider call limit reached"),
        "{error}"
    );
}
