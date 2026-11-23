//! Native harness configuration unit tests covering model listing, provider
//! registration, sign-in, environments, and session storage.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_contracts::{PermissionMode, ProviderCallBudget, ReasoningLevel};
use ag_harness::catalog::ModelId;
use ag_harness::lifecycle::LifecycleEvent;
use ag_harness::model::{
    ModelCapabilities, ModelClient, ModelMetadata, ModelProfile, ModelRegistry, ModelRequest,
    ReasoningEffort,
};
use ag_harness::provider::{AnthropicConfig, CostTier, ModelConfiguration, ModelProvider};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{MemoryStore, SessionStore};
use ag_harness::{
    Harness, HarnessBuildError, ImageContent, ImageMediaType, InputBlock, Model, TurnInput,
};
use ag_protocol::ProtocolRequestProfile;
use ag_session::{AgentModel, HarnessModelOption, HarnessModelStatus, ModelCost, ServedModel};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{
    BudgetedModel, FALLBACK_MODEL_PROFILE, HarnessContext, ModelSelection, NativeHarnessConfig,
    ResumeOnlyModel, RunOptions, bash_environment, chatgpt_auth_file, configuration_status,
    harness_models, model_cost, output_schema, provider_display_name, reasoning_effort, repository,
    running_context_window, unicode_environment,
};
use crate::agent::model_profile;
use crate::agent::native::sandbox::{CommandSandbox, SESSION_SANDBOX};

#[test]
/// Asks for the base URL of a keyed provider without a default endpoint,
/// such as Qwen, while Kimi's default endpoint needs none.
fn configuration_status_requires_a_base_url_only_without_a_default() {
    // Arrange
    let environment = environment(&[("DASHSCOPE_API_KEY", "key"), ("KIMI_API_KEY", "key")]);

    // Act
    let qwen = configuration_status(ModelProvider::Qwen, &environment, |_| false);
    let kimi = configuration_status(ModelProvider::Kimi, &environment, |_| false);

    // Assert
    assert_eq!(
        qwen,
        HarnessModelStatus::MissingConfiguration("DASHSCOPE_BASE_URL")
    );
    assert_eq!(kimi, HarnessModelStatus::Ready);
}

#[test]
/// Locates Agentty's `ChatGPT` sign-in from the named file, then its data
/// directory, then the home directory.
fn chatgpt_auth_file_prefers_the_named_file_then_agentty_root_then_home() {
    // Arrange
    let named = environment(&[
        ("CHATGPT_AUTH_FILE", "/tmp/sign-in.json"),
        ("AGENTTY_ROOT", "/data/agentty"),
    ]);
    let root = environment(&[
        ("CHATGPT_AUTH_FILE", " "),
        ("AGENTTY_ROOT", "/data/agentty"),
    ]);
    // A data directory is used as given, as `agentty`'s `home.rs` reads it.
    let spaced_root = environment(&[("AGENTTY_ROOT", "/data ")]);
    let home = environment(&[("AGENTTY_ROOT", ""), ("HOME", "/home/user")]);

    // Act
    let locations = [
        chatgpt_auth_file(&named),
        chatgpt_auth_file(&root),
        chatgpt_auth_file(&spaced_root),
        chatgpt_auth_file(&home),
        chatgpt_auth_file(&BTreeMap::new()),
    ];

    // Assert
    assert_eq!(
        locations,
        [
            Some(PathBuf::from("/tmp/sign-in.json")),
            Some(PathBuf::from("/data/agentty/auth/chatgpt.json")),
            Some(PathBuf::from("/data /auth/chatgpt.json")),
            Some(PathBuf::from("/home/user/.agentty/auth/chatgpt.json")),
            None,
        ]
    );
}

#[test]
/// Registers a `ChatGPT` model from the sign-in under `AGENTTY_ROOT` alone,
/// since the harness keeps no default credential location.
fn chatgpt_registry_reads_the_sign_in_under_agentty_root() {
    // Arrange
    let (directory, auth_file) = signed_in_auth_file();
    let root = tempfile::tempdir().expect("data root");
    std::fs::create_dir_all(root.path().join("auth")).expect("auth directory");
    std::fs::copy(&auth_file, root.path().join("auth/chatgpt.json")).expect("sign-in");
    drop(directory);
    let root_value = root.path().to_str().expect("UTF-8 path");
    let environment = environment(&[("AGENTTY_ROOT", root_value)]);
    let selection =
        ModelSelection::resolve("chatgpt/gpt-6.1-sol", &environment).expect("served model");

    // Act
    let registry = selection.registry(&environment, None, None);

    // Assert
    let registry = registry.expect("registry");
    let registration = registry.resolve(&selection.key()).expect("registration");
    assert_eq!(registration.identity().revision(), "agentty-native-2");
}

#[test]
/// Summarizes on the requested model only when it is another harness model
/// that is ready now and declares structured output; every other request
/// falls back to the session's model.
fn summary_model_falls_back_to_the_session_model() {
    // Arrange
    let environment = environment(&[
        ("DASHSCOPE_API_KEY", "key"),
        ("DASHSCOPE_BASE_URL", "http://qwen"),
        ("MODEL_API_KEY", "key"),
        ("OPENAI_API_KEY", "sk-key"),
        ("CHATGPT_AUTH_FILE", "/tmp/chatgpt.json"),
    ]);
    let session = ModelSelection::resolve("muse-spark-1.3", &environment).expect("session");
    let cases: [(Option<&str>, bool, Option<&str>); 10] = [
        // Unset: the session model compacts.
        (None, true, None),
        // A ready model with structured output compacts.
        (Some("gpt-5.6-terra"), false, Some("openai/gpt-5.6-terra")),
        // Credentials missing.
        (Some("claude-haiku-5-5"), true, None),
        // Ready, but without structured output.
        (Some("qwen3.8-flash"), true, None),
        // A local server's model declares no structured output.
        (Some("openai-compatible/qwen3:latest"), true, None),
        // The session's own model.
        (Some("muse-spark-1.3"), true, None),
        // A model no harness provider serves, such as an agent CLI's.
        (Some("gemini-4-argon"), true, None),
        (Some(""), true, None),
        // A sign-in provider compacts only while signed in.
        (Some("chatgpt/gpt-6-luna"), false, None),
        (Some("chatgpt/gpt-6-luna"), true, Some("chatgpt/gpt-6-luna")),
    ];

    for (requested, signed_in, expected) in cases {
        // Act
        let summary = session.summary_model(requested, &environment, |_| signed_in);

        // Assert
        assert_eq!(
            summary.as_ref().map(ModelSelection::key).as_deref(),
            expected,
            "{requested:?}"
        );
    }
}

#[test]
/// Names a model as users pick it: `ChatGPT` plan and local server models
/// with their provider, other cataloged models bare, and unknown keys as
/// given.
fn model_ids_keep_the_provider_users_pick_them_by() {
    // Arrange
    let keys = [
        "chatgpt/gpt-6-luna",
        "openai/gpt-6-luna",
        "openai-compatible/qwen3:latest",
        "muse/muse-spark-1.3",
        "host/unknown",
    ];

    // Act
    let ids = keys.map(ModelSelection::model_id_of);

    // Assert
    assert_eq!(
        ids,
        [
            "chatgpt/gpt-6-luna",
            "gpt-6-luna",
            "openai-compatible/qwen3:latest",
            "muse-spark-1.3",
            "host/unknown",
        ]
    );
}

#[test]
/// Builds a harness that summarizes on a usable summary model, and one that
/// summarizes on its own model when the summary model's client cannot be
/// built or a session refuses it, without failing either.
fn harness_summarizes_on_a_usable_summary_model() {
    // Arrange
    let worktree = tempfile::tempdir().expect("worktree");
    let real_path = std::env::var("PATH").expect("test PATH should be set");
    let keyed = environment(&[
        ("MODEL_API_KEY", "key"),
        ("OPENAI_API_KEY", "sk-key"),
        ("PATH", &real_path),
    ]);
    let unkeyed_summary = environment(&[("MODEL_API_KEY", "key"), ("PATH", &real_path)]);
    let repository = repository(worktree.path(), &keyed).expect("repository");
    let session = ModelSelection::resolve("muse-spark-1.3", &keyed).expect("session");
    let summary = ModelSelection::resolve("gpt-5.6-terra", &keyed).expect("summary");
    let context = |environment| HarnessContext {
        compacts: true,
        environment,
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Medium,
        repository: &repository,
        running_context_window: None,
        sandbox: SESSION_SANDBOX,
        summary_model: Some(&summary),
    };

    // Act
    let summarizing = session.harness(&context(&keyed), |_: LifecycleEvent| {});
    let unsummarized = session.harness(&context(&unkeyed_summary), |_: LifecycleEvent| {});
    let mut refused = false;
    context(&keyed).apply_summary_model(|_| {
        refused = true;

        Err(HarnessBuildError::ModelRequired.into())
    });
    let unlisted = ModelSelection::resolve("openai-compatible/local", &keyed)
        .expect("served model")
        .summary_entry(&keyed, None);

    // Assert
    let summarizing = summarizing.expect("harness with a summary model");
    let unsummarized = unsummarized.expect("harness without a summary model");
    assert_eq!(
        summarizing.summary_model().map(ModelId::as_str),
        Some("openai/gpt-5.6-terra")
    );
    assert_eq!(unsummarized.summary_model(), None);
    assert!(refused);
    let key = |harness: &Harness| {
        harness
            .model_registration()
            .map(|registration| registration.identity().key().to_string())
    };
    // The summary model never changes the recorded registration.
    assert_eq!(key(&summarizing).as_deref(), Some("muse/muse-spark-1.3"));
    assert_eq!(key(&unsummarized), key(&summarizing));
    assert!(
        unlisted
            .err()
            .is_some_and(|error| error.contains("does not list")),
    );
}

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

/// Writes a stored `ChatGPT` sign-in and returns its directory and file path.
fn signed_in_auth_file() -> (tempfile::TempDir, String) {
    let directory = tempfile::tempdir().expect("auth directory");
    let auth_file = directory.path().join("chatgpt.json");
    let credentials = json!({
        "access_token": "access",
        "client_id": "client",
        "email": null,
        "expires_at": u64::MAX,
        "ext_agent_host_id": "host",
        "id_token": "id",
        "refresh_token": "refresh",
        "scopes": []
    });
    std::fs::write(&auth_file, credentials.to_string()).expect("credential file");
    let auth_file = auth_file.to_str().expect("UTF-8 path").to_string();

    (directory, auth_file)
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
    let options = harness_models(&environment, |_| false).await;

    // Assert
    let summary = options
        .iter()
        .map(|option| (option.model, option.cost, option.status))
        .collect::<Vec<_>>();
    let anthropic = HarnessModelStatus::MissingConfiguration("ANTHROPIC_API_KEY");
    let dashscope = HarnessModelStatus::MissingConfiguration("DASHSCOPE_API_KEY");
    let openai = HarnessModelStatus::MissingConfiguration("OPENAI_API_KEY");
    let ready = HarnessModelStatus::Ready;
    let sign_in = HarnessModelStatus::SignInRequired("ag-harness login chatgpt");
    assert_eq!(
        summary,
        vec![
            (served("qwen3:latest"), ModelCost::Local, ready),
            (AgentModel::MuseSpark13, ModelCost::Standard, ready),
            (AgentModel::MuseSpark13Contributor, ModelCost::Cheap, ready),
            // Kimi defaults to Moonshot AI's international endpoint.
            (AgentModel::KimiK3, ModelCost::Premium, ready),
            (AgentModel::KimiK26, ModelCost::Standard, ready),
            (AgentModel::Qwen38Max, ModelCost::Standard, dashscope),
            (AgentModel::QwenPlus, ModelCost::Cheap, dashscope),
            (AgentModel::Qwen38Flash, ModelCost::Cheap, dashscope),
            (AgentModel::Qwen37Flash, ModelCost::Cheap, dashscope),
            (AgentModel::ChatGptGpt61Sol, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt61Terra, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt6Astra, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt6Sol, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt6Luna, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt56Sol, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt56Terra, ModelCost::Plan, sign_in),
            (AgentModel::ChatGptGpt56Luna, ModelCost::Plan, sign_in),
            (AgentModel::Gpt61Sol, ModelCost::Standard, openai),
            (AgentModel::Gpt6Astra, ModelCost::Premium, openai),
            (AgentModel::Gpt6Luna, ModelCost::Cheap, openai),
            (AgentModel::Gpt56Terra, ModelCost::Standard, openai),
            (AgentModel::ClaudeSonnet55, ModelCost::Standard, anthropic),
            (AgentModel::ClaudeOpus55, ModelCost::Premium, anthropic),
            (AgentModel::ClaudeFable52, ModelCost::Premium, anthropic),
            (AgentModel::ClaudeHaiku55, ModelCost::Cheap, anthropic),
        ]
    );
    assert_eq!(options[0].provider, "OpenAI-compatible server");
    assert_eq!(options[1].provider, "Muse");
    assert_eq!(options[3].provider, "Kimi");
    assert_eq!(options[5].provider, "Qwen");
    assert_eq!(options[9].provider, "ChatGPT plan");
    assert_eq!(options[17].provider, "OpenAI");
    let sonnet = options
        .iter()
        .find(|option| option.model == AgentModel::ClaudeSonnet55)
        .expect("Claude Sonnet is listed");
    assert_eq!(sonnet.provider, "Anthropic");
    // Only Muse's contributor tier lets the provider train on prompts.
    assert_eq!(
        options
            .iter()
            .filter(|option| option.trains_on_prompts)
            .map(|option| option.model)
            .collect::<Vec<_>>(),
        [AgentModel::MuseSpark13Contributor]
    );
    // The harness catalog declares the ChatGPT-plan windows.
    assert_eq!(options[9].context_window, Some(500_000));
    // The harness catalog declares the Qwen flash window, and Agentty its
    // fitted Qwen3.8-Max and Qwen-Plus profiles.
    assert_eq!(options[7].context_window, Some(1_000_000));
    assert_eq!(options[5].context_window, Some(1_000_000));
    assert_eq!(options[6].context_window, Some(1_000_000));
    // The harness catalog declares the Muse and Kimi windows.
    assert_eq!(options[1].context_window, Some(1_048_576));
    assert_eq!(options[4].context_window, Some(262_144));
    // Shared Codex models show the window the harness runs them with.
    assert_eq!(
        options[17].context_window,
        ModelProvider::OpenAi
            .declared_profile("gpt-6.1-sol", |_| Err(std::env::VarError::NotPresent))
            .expect("OpenAI profiles read no environment")
            .map(ModelProfile::context_window_tokens)
    );
}

#[tokio::test]
/// Marks ChatGPT-plan models ready once the credential file the environment
/// locates exists, and asks for sign-in otherwise.
async fn harness_models_report_the_chatgpt_sign_in() {
    // Arrange
    let environment = environment(&[
        ("CHATGPT_AUTH_FILE", "/credentials/chatgpt.json"),
        ("OPENAI_COMPATIBLE_BASE_URL", &unreachable_base_url()),
    ]);
    let plan_statuses = |options: Vec<HarnessModelOption>| {
        options
            .into_iter()
            .filter(|option| option.cost == ModelCost::Plan)
            .map(|option| option.status)
            .collect::<Vec<_>>()
    };

    // Act
    let signed_in = harness_models(&environment, |path| {
        path == Path::new("/credentials/chatgpt.json")
    })
    .await;
    let signed_out = harness_models(&environment, |_| false).await;

    // Assert
    assert_eq!(plan_statuses(signed_in), [HarnessModelStatus::Ready; 8]);
    assert_eq!(
        plan_statuses(signed_out),
        [HarnessModelStatus::SignInRequired("ag-harness login chatgpt"); 8]
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
        [
            "Muse",
            "Kimi",
            "Qwen",
            "OpenAI",
            "ChatGPT plan",
            "Anthropic",
            "OpenAI-compatible server"
        ]
    );
}

#[test]
/// Shows every catalog price class as the matching model cost.
fn catalog_cost_tiers_map_to_model_costs() {
    // Arrange
    let tiers = [
        CostTier::Local,
        CostTier::Plan,
        CostTier::Cheap,
        CostTier::Standard,
        CostTier::Premium,
    ];

    // Act
    let costs = tiers.map(model_cost);

    // Assert
    assert_eq!(
        costs,
        [
            ModelCost::Local,
            ModelCost::Plan,
            ModelCost::Cheap,
            ModelCost::Standard,
            ModelCost::Premium
        ]
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
    let options = harness_models(&environment, |_| false).await;

    // Assert
    let local = options
        .iter()
        .filter(|option| !option.cost.is_paid())
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
/// declares and tool calls but no image input, without an API key, and
/// reports invalid declarations.
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
    assert!(registration.capabilities().tool_calls);
    assert!(!registration.capabilities().image_input);
    assert_eq!(
        registration
            .metadata()
            .as_ref()
            .map(ModelMetadata::provider),
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
    assert!(ModelSelection::resolve("claude-unlisted", &environment(&[])).is_err());
}

#[test]
/// Routes `chatgpt/<model>` ids only to the `ChatGPT` plan and bare ids only to
/// API-billed providers, so neither silently stands in for the other.
fn chatgpt_selection_never_shares_ids_with_openai() {
    // Arrange
    let environment = environment(&[]);

    // Act
    let plan = ModelSelection::resolve("chatgpt/gpt-6.1-sol", &environment)
        .expect("a plan model should resolve");
    let api =
        ModelSelection::resolve("gpt-6.1-sol", &environment).expect("an API model should resolve");
    let plan_only_bare = ModelSelection::resolve("gpt-6.1-terra", &environment);
    let unknown_plan = ModelSelection::resolve("chatgpt/kimi-k3", &environment);

    // Assert
    assert_eq!(plan.provider, ModelProvider::ChatGpt);
    assert_eq!(plan.model, "gpt-6.1-sol");
    assert_eq!(plan.key(), "chatgpt/gpt-6.1-sol");
    assert_eq!(api.provider, ModelProvider::OpenAi);
    assert_eq!(api.key(), "openai/gpt-6.1-sol");
    assert!(plan_only_bare.is_err());
    assert_eq!(
        unknown_plan,
        Err("The harness does not serve model `chatgpt/kimi-k3`.".to_string())
    );
    assert_eq!(ModelSelection::from_key(&plan.key()), Some(plan));
    assert_eq!(ModelSelection::from_key(&api.key()), Some(api));
    assert_eq!(ModelSelection::from_key("openai/gpt-6.1-terra"), None);
    assert_eq!(ModelSelection::from_key("chatgpt/kimi-k3"), None);
}

#[test]
/// Asks for sign-in, not an API key, when the `ChatGPT` plan has none stored.
fn chatgpt_registration_without_sign_in_names_the_login_command() {
    // Arrange
    let directory = tempfile::tempdir().expect("auth directory");
    let missing = directory.path().join("chatgpt.json");
    let environment = environment(&[("CHATGPT_AUTH_FILE", missing.to_str().expect("UTF-8"))]);
    let selection =
        ModelSelection::resolve("chatgpt/gpt-6-luna", &environment).expect("plan model");

    // Act
    let error = selection
        .registry(&environment, None, None)
        .err()
        .expect("a missing sign-in should fail");

    // Assert
    assert!(
        error.starts_with("Cannot use harness model `gpt-6-luna`"),
        "{error}"
    );
    assert!(
        error.ends_with("Run `ag-harness login chatgpt`, then retry."),
        "{error}"
    );
    assert!(!error.contains("OPENAI_API_KEY"), "{error}");
}

#[test]
/// Resumes a ChatGPT-plan session without a stored sign-in.
fn chatgpt_resume_only_harness_needs_no_sign_in() {
    // Arrange
    let worktree = tempfile::tempdir().expect("worktree");
    let real_path = std::env::var("PATH").expect("test PATH should be set");
    let environment = environment(&[("PATH", &real_path)]);
    let repository = repository(worktree.path(), &environment).expect("repository");
    let selection =
        ModelSelection::resolve("chatgpt/gpt-6.1-sol", &environment).expect("plan model");
    let context = HarnessContext {
        compacts: false,
        sandbox: SESSION_SANDBOX,
        summary_model: None,
        environment: &environment,
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Medium,
        repository: &repository,
        running_context_window: None,
    };

    // Act
    let harness = selection.resume_only_harness(&context, |_: LifecycleEvent| {});

    // Assert
    let harness = harness.expect("resume-only harness");
    let registration = harness.model_registration().expect("registration");
    assert_eq!(registration.identity().key(), "chatgpt/gpt-6.1-sol");
    assert_eq!(registration.identity().revision(), "agentty-native-2");
    assert!(registration.capabilities().tool_calls);
}

#[test]
/// Registers each served model under the current revision with its declared
/// context profile: the provider catalog's where it declares one, Agentty's
/// for the others, and the conservative fallback for uncataloged models.
/// Every model declares tool calls, and all but the text-only Qwen3.7-Flash
/// declare image input.
fn registrations_declare_revision_profiles_and_capabilities() {
    // Arrange
    let (_directory, auth_file) = signed_in_auth_file();
    let environment = environment(&[
        ("DASHSCOPE_API_KEY", "key"),
        ("DASHSCOPE_BASE_URL", "http://qwen"),
        ("ANTHROPIC_API_KEY", "key"),
        ("KIMI_API_KEY", "key"),
        ("KIMI_BASE_URL", "http://kimi"),
        ("MODEL_API_KEY", "key"),
        ("OPENAI_API_KEY", "sk-key"),
        ("CHATGPT_AUTH_FILE", &auth_file),
    ]);
    let models = ModelProvider::all().iter().flat_map(|provider| {
        provider
            .known_models()
            .iter()
            .map(move |model| (*provider, *model))
    });

    // Act / Assert
    for (provider, model) in models {
        let id = if provider == ModelProvider::ChatGpt {
            format!("chatgpt/{model}")
        } else {
            model.to_string()
        };
        let selection = ModelSelection::resolve(&id, &environment).expect("served model");
        let registry = selection
            .registry(&environment, None, None)
            .expect("registry");
        let registration = registry.resolve(&selection.key()).expect("registration");
        // The harness catalog's own profile wins over Agentty's fitted one.
        let expected = provider
            .declared_profile(model, |_| Err(std::env::VarError::NotPresent))
            .expect("hosted catalog profiles read no environment")
            .or_else(|| {
                AgentModel::parse_persisted(&id)
                    .ok()
                    .and_then(model_profile::declared)
            })
            .unwrap_or(FALLBACK_MODEL_PROFILE);
        assert_eq!(registration.identity().key(), selection.key());
        assert_eq!(registration.identity().revision(), "agentty-native-2");
        let spec = registration.catalog_spec().expect("catalog entry");
        assert_eq!(spec.id().as_str(), selection.key());
        assert_eq!(
            (spec.pricing(), spec.max_output(), spec.max_concurrency()),
            (None, None, None),
            "runs stay unpriced and unmetered"
        );
        assert_eq!(registration.capabilities().profile, expected, "{model}");
        assert!(registration.capabilities().tool_calls, "{model}");
        assert_eq!(
            registration.capabilities().image_input,
            model != "qwen3.7-flash",
            "{model}"
        );
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
        ("ANTHROPIC_API_KEY", "secret"),
        ("ANTHROPIC_BASE_URL", "https://proxy.example/v1"),
        ("CHATGPT_AUTH_FILE", "/home/test/chatgpt.json"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "secret"),
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

#[tokio::test]
/// Gives each session one stable temporary directory beside its harness
/// database, and rejects invalid session ids.
async fn session_sandbox_keeps_one_temporary_directory_per_session() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let workspace = tempfile::tempdir().expect("workspace");
    let config = NativeHarnessConfig::new(data_root.path().to_path_buf());
    let environment = BTreeMap::new();

    // Act
    let first = config
        .session_sandbox("session-1", workspace.path(), &environment)
        .await;
    let second = config
        .session_sandbox("session-1", workspace.path(), &environment)
        .await;
    let other = config
        .session_sandbox("session-2", workspace.path(), &environment)
        .await;
    let invalid = config
        .session_sandbox("a/b", workspace.path(), &environment)
        .await;

    // Assert
    let policy = |sandbox: CommandSandbox| {
        RunOptions::new(
            ProtocolRequestProfile::SessionTurn,
            PermissionMode::AutoEdit,
            &environment,
            &sandbox,
        )
        .expect("options")
        .bash
    };
    let first = policy(first.expect("first sandbox"));
    assert_eq!(first, policy(second.expect("second sandbox")));
    assert_ne!(first, policy(other.expect("other session sandbox")));
    assert!(data_root.path().join("session-1/tmp/cargo").is_dir());
    assert!(invalid.is_err());
}

#[tokio::test]
/// Gives each utility prompt a fresh temporary directory, removed when the
/// prompt ends; the first prompt of a process removes those an earlier
/// process left behind; a data root that cannot hold one is reported.
async fn utility_sandbox_creates_a_temporary_directory_per_prompt() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let stale = data_root.path().join(".utility/run-stale");
    std::fs::create_dir_all(&stale).expect("stale directory");
    let workspace = tempfile::tempdir().expect("workspace");
    let config = NativeHarnessConfig::new(data_root.path().to_path_buf());
    let file_root = tempfile::NamedTempFile::new().expect("file in place of a directory");
    let unusable = NativeHarnessConfig::new(file_root.path().to_path_buf());
    let environment = BTreeMap::new();

    // Act
    let (_sandbox, first) = config
        .utility_sandbox(workspace.path(), &environment)
        .await
        .expect("first utility sandbox");
    std::fs::create_dir_all(&stale).expect("directory of a running prompt");
    let (_sandbox, second) = config
        .utility_sandbox(workspace.path(), &environment)
        .await
        .expect("second utility sandbox");
    let failure = unusable
        .utility_sandbox(workspace.path(), &environment)
        .await;

    // Assert
    let first_path = first.path().to_path_buf();
    assert_eq!(
        first_path.parent(),
        Some(data_root.path().join(".utility").as_path())
    );
    assert_ne!(first_path, second.path());
    assert!(stale.is_dir(), "only the first prompt of a process sweeps");
    drop(first);
    assert!(!first_path.exists());
    assert!(
        failure
            .err()
            .expect("a file root should fail")
            .contains("Failed to create the harness temporary directory")
    );
}

#[tokio::test]
/// Reports a session temporary directory that cannot be created.
async fn session_sandbox_reports_temporary_directory_failures() {
    // Arrange
    let data_root = tempfile::tempdir().expect("data root");
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(data_root.path().join("session-1")).expect("session directory");
    std::fs::write(data_root.path().join("session-1/tmp"), "").expect("file in place");
    let config = NativeHarnessConfig::new(data_root.path().to_path_buf());

    // Act
    let failure = config
        .session_sandbox("session-1", workspace.path(), &BTreeMap::new())
        .await;

    // Assert
    assert!(
        failure
            .err()
            .expect("a file in place of the directory should fail")
            .contains("Failed to create the harness temporary directory")
    );
}

#[test]
/// Withholds credential-like variables from commands, whatever their case,
/// and keeps ordinary ones.
fn bash_environment_drops_credentials() {
    // Arrange
    let credentials = [
        "AWS_ACCESS_KEY_ID",
        "AWS_PROFILE",
        "CARGO_REGISTRY_TOKEN",
        "DATABASE_URL",
        "GEMINI_API_KEY",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "GOOGLE_API_KEY",
        "HF_TOKEN",
        "NPM_TOKEN",
        "SENTRY_DSN",
        "app_secret",
        "DB_PASSWORD",
        "SMTP_PASSWD",
        "GOOGLE_APPLICATION_CREDENTIALS",
    ];
    let kept = [
        "CARGO_TARGET_DIR",
        "EDITOR",
        "KEYBOARD_LAYOUT",
        "SSH_AUTH_SOCK",
    ];
    let environment = credentials
        .iter()
        .chain(&kept)
        .map(|name| (name.to_string(), "value".to_string()))
        .collect::<BTreeMap<_, _>>();

    // Act
    let granted = bash_environment(&environment)
        .into_iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        granted,
        [
            "SSH_AUTH_SOCK",
            "CARGO_TARGET_DIR",
            "EDITOR",
            "KEYBOARD_LAYOUT"
        ]
    );
}

#[test]
/// Grants the sandbox's own `TMPDIR` and tool caches in place of the host's.
fn bash_policies_replace_host_temporary_and_cache_directories() {
    // Arrange
    let sandbox = CommandSandbox::new(
        Path::new("/data-root"),
        PathBuf::from("/data-root/session-1/tmp"),
        &BTreeMap::new(),
    );
    let host = environment(&[
        ("CARGO_HOME", "/home/test/.cargo"),
        ("PATH", "/usr/bin"),
        ("TMPDIR", "/tmp/host"),
        ("XDG_CACHE_HOME", "/home/test/.cache"),
    ]);
    let without_host_values = environment(&[("PATH", "/usr/bin")]);
    let policy = |environment: &BTreeMap<String, String>| {
        RunOptions::new(
            ProtocolRequestProfile::SessionTurn,
            PermissionMode::AutoEdit,
            environment,
            &sandbox,
        )
        .expect("options")
        .bash
    };

    // Act
    let with_host = policy(&host);
    let without_host = policy(&without_host_values);

    // Assert
    assert_eq!(with_host, without_host);
}

#[test]
/// Skips variables beyond the harness grant limit instead of failing.
fn run_options_tolerate_oversized_environments() {
    // Arrange
    let environment = (0..100)
        .map(|index| (format!("VAR_{index:03}"), "value".to_string()))
        .collect::<BTreeMap<_, _>>();
    let sandbox = CommandSandbox::new(
        Path::new("/data-root"),
        PathBuf::from("/data-root/session-1/tmp"),
        &environment,
    );

    // Act
    let edit = RunOptions::new(
        ProtocolRequestProfile::SessionTurn,
        PermissionMode::AutoEdit,
        &environment,
        &sandbox,
    );
    let read_only = RunOptions::new(
        ProtocolRequestProfile::SessionTurn,
        PermissionMode::ReadOnly,
        &environment,
        &sandbox,
    );

    // Assert
    let edit = edit.expect("edit options should build");
    assert!(edit.bash.is_some());
    assert_eq!(
        edit.tools.names().collect::<Vec<_>>(),
        ["read", "write", "bash"]
    );
    let read_only = read_only.expect("read-only options should build");
    assert!(read_only.bash.is_none());
    assert_eq!(read_only.tools.names().collect::<Vec<_>>(), ["read"]);
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
    let png_signature = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n'];
    let image = ImageContent::new(ImageMediaType::Png, png_signature).expect("image");
    let image_input = TurnInput::from_blocks(vec![
        InputBlock::Text("Describe this.".to_string()),
        InputBlock::Image(image),
    ])
    .expect("image input");

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
    // Muse reads images, so the registered image input reaches its client.
    assert!(model.validate_input(&image_input).is_ok());
    assert!(!model.keeps_prefix_across_schemas());
    assert!(!model.replays_reasoning_text());
    assert!(!model.replays_turn_instructions());
    let error = completion.expect_err("an exhausted budget should refuse the request");
    assert!(
        error.to_string().contains("provider call limit reached"),
        "{error}"
    );
}

#[test]
/// Serves Claude ids shared with the Claude Code backend through the
/// Anthropic provider, under an `anthropic/<model>` key, with or without a
/// provider-call budget and without credentials when only resuming.
fn claude_models_register_on_the_anthropic_provider() {
    // Arrange
    let worktree = tempfile::tempdir().expect("worktree");
    let real_path = std::env::var("PATH").expect("test PATH should be set");
    let keyed = environment(&[("ANTHROPIC_API_KEY", "key"), ("PATH", &real_path)]);
    let unkeyed = environment(&[("PATH", &real_path)]);
    let repository = repository(worktree.path(), &unkeyed).expect("repository");
    let budget = ProviderCallBudget::new(1);
    let budgeted_client = BudgetedModel {
        budget: budget.clone(),
        inner: ModelConfiguration::new(ModelProvider::Anthropic, "claude-opus-5-5")
            .client_from_environment(|name| {
                keyed
                    .get(name)
                    .cloned()
                    .ok_or(std::env::VarError::NotPresent)
            })
            .expect("client should build"),
    };
    let context = HarnessContext {
        compacts: false,
        sandbox: SESSION_SANDBOX,
        summary_model: None,
        environment: &unkeyed,
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::Medium,
        repository: &repository,
        running_context_window: None,
    };

    // Act
    let selection = ModelSelection::resolve("claude-opus-5-5", &keyed).expect("served model");
    let unbudgeted = selection.registry(&keyed, None, None).expect("registry");
    let budgeted = selection
        .registry(&keyed, Some(&budget), None)
        .expect("budgeted registry");
    let missing_key = selection.registry(&unkeyed, None, None);
    let resume_only = selection.resume_only_harness(&context, |_: LifecycleEvent| {});

    // Assert
    assert_eq!(selection.key(), "anthropic/claude-opus-5-5");
    assert_eq!(
        ModelSelection::from_key("anthropic/claude-opus-5-5").as_ref(),
        Some(&selection)
    );
    assert_eq!(ModelSelection::from_key("anthropic/claude-unlisted"), None);
    for registry in [&unbudgeted, &budgeted] {
        let registration = registry
            .resolve("anthropic/claude-opus-5-5")
            .expect("registration");
        assert_eq!(registration.identity().revision(), "agentty-native-2");
        assert_eq!(
            registration.capabilities().profile.context_window_tokens(),
            1_000_000
        );
        assert!(registration.capabilities().image_input);
    }
    // Anthropic keeps signed thinking valid only when history replays each
    // turn's instructions, which a provider-call budget must not hide.
    assert!(budgeted_client.replays_turn_instructions());
    let error = missing_key.err().expect("a missing key should be reported");
    assert!(
        error.ends_with("Set ANTHROPIC_API_KEY before starting Agentty."),
        "{error}"
    );
    let resume_only = resume_only.expect("resume-only harness");
    let registration = resume_only.model_registration().expect("registration");
    assert_eq!(registration.identity().key(), "anthropic/claude-opus-5-5");
}

#[test]
/// Declares the same key, revision, and capabilities with a provider-call
/// budget as without one.
fn budgeted_registration_keeps_the_identity_and_capabilities() {
    // Arrange
    let environment = environment(&[("MODEL_API_KEY", "key")]);
    let selection = ModelSelection::resolve("muse-spark-1.3", &environment).expect("served model");
    let budget = ProviderCallBudget::new(1);

    // Act
    let registry = selection
        .registry(&environment, Some(&budget), None)
        .expect("registry");

    // Assert
    let registration = registry
        .resolve("muse/muse-spark-1.3")
        .expect("registration");
    assert_eq!(registration.identity().revision(), "agentty-native-2");
    assert!(registration.capabilities().tool_calls);
    assert!(registration.capabilities().image_input);
}

#[tokio::test]
/// Resumes a session recorded on the hand-built registration of earlier
/// releases, which declared the same `provider/model` key, revision, and
/// capabilities the catalog registration now derives.
async fn sessions_recorded_on_manual_registrations_resume_on_catalog_registrations() {
    // Arrange
    let environment = environment(&[("KIMI_API_KEY", "key"), ("KIMI_BASE_URL", "http://kimi")]);
    let selection = ModelSelection::resolve("kimi-k3", &environment).expect("served model");
    let registry = selection
        .registry(&environment, None, None)
        .expect("registry");
    let current = registry.resolve("kimi/kimi-k3").expect("registration");
    // Earlier releases declared the provider catalog's profile for Kimi K3.
    let recorded_profile = ModelProvider::Kimi
        .declared_profile("kimi-k3", |_| Err(std::env::VarError::NotPresent))
        .expect("hosted catalog profiles read no environment")
        .expect("Kimi K3 declares a profile");
    let capabilities = ModelCapabilities {
        image_input: true,
        tool_calls: true,
        ..ModelCapabilities::new(recorded_profile)
    };
    let mut recorded = ModelRegistry::new();
    recorded
        .register(
            ExecutionIdentity::new("kimi/kimi-k3", "agentty-native-2").expect("identity"),
            ResumeOnlyModel {
                metadata: current.metadata().expect("metadata"),
            },
            capabilities,
        )
        .expect("recorded registration");
    let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
    Harness::from_registry(&recorded, "kimi/kimi-k3")
        .expect("recorded harness")
        .store(Arc::clone(&store))
        .session(
            "recorded",
            output_schema(ProtocolRequestProfile::SessionTurn).expect("schema"),
        )
        .create()
        .await
        .expect("recorded session");

    // Act
    let resumed = Harness::from_registry(&registry, &selection.key())
        .expect("harness")
        .store(store)
        .resume("recorded")
        .await;

    // Assert
    assert_eq!(
        current.identity(),
        recorded
            .resolve("kimi/kimi-k3")
            .expect("recorded")
            .identity()
    );
    assert_eq!(current.capabilities(), capabilities);
    resumed.expect("the catalog registration resumes the recorded session");
}

#[tokio::test]
/// Sends every protocol schema to Claude using only the JSON Schema keywords
/// its structured outputs accept, which the API otherwise answers with a
/// `400`.
async fn protocol_schemas_send_only_keywords_claude_accepts() {
    for profile in [
        ProtocolRequestProfile::FocusedReview,
        ProtocolRequestProfile::GraphProposal,
        ProtocolRequestProfile::ReviewMetadata,
        ProtocolRequestProfile::SessionTurn,
        ProtocolRequestProfile::UtilityPrompt,
    ] {
        // Arrange
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({})))
            .mount(&server)
            .await;
        let client = ModelClient::anthropic(AnthropicConfig {
            api_key: "claude-key".to_string(),
            base_url: format!("{}/v1", server.uri()),
            model: "claude-haiku-5-5".to_string(),
        })
        .expect("client");
        let schema = output_schema(profile).expect("schema");

        // Act
        let _rejected = client.complete(ModelRequest::new("Answer", schema)).await;

        // Assert
        let requests = server.received_requests().await.expect("recording");
        let body: Value = requests[0].body_json().expect("JSON request body");
        assert_claude_accepts(
            &body["output_config"]["format"]["schema"],
            &format!("{profile:?}"),
        );
    }
}

/// Asserts that `schema`, found at `at`, uses only the keywords, values, and
/// combinator shapes Anthropic documents as supported for structured
/// outputs: an `anyOf` only beside annotations, and no `allOf` of two or
/// more schemas where one directly holds `$ref`, `anyOf`, or `allOf`.
fn assert_claude_accepts(schema: &Value, at: &str) {
    const ANY_OF_SIBLINGS: [&str; 6] = [
        "$defs",
        "anyOf",
        "default",
        "definitions",
        "description",
        "title",
    ];
    const FORMATS: [&str; 10] = [
        "date",
        "date-time",
        "duration",
        "email",
        "hostname",
        "ipv4",
        "ipv6",
        "time",
        "uri",
        "uuid",
    ];
    let Some(object) = schema.as_object() else {
        return;
    };
    if object.contains_key("anyOf") {
        let siblings: Vec<_> = object
            .keys()
            .filter(|keyword| !ANY_OF_SIBLINGS.contains(&keyword.as_str()))
            .collect();
        assert!(siblings.is_empty(), "{at}: `anyOf` beside {siblings:?}");
    }
    if let Some(branches) = object
        .get("allOf")
        .and_then(Value::as_array)
        .filter(|branches| branches.len() > 1)
    {
        assert!(
            branches.iter().all(|branch| ["$ref", "anyOf", "allOf"]
                .iter()
                .all(|keyword| branch.get(keyword).is_none())),
            "{at}: `allOf` of several schemas holds `$ref`, `anyOf`, or `allOf`"
        );
    }
    for (keyword, value) in object {
        let at = format!("{at}/{keyword}");
        match keyword.as_str() {
            "$defs" | "definitions" | "properties" => {
                for (name, nested) in value.as_object().expect("schema map") {
                    assert_claude_accepts(nested, &format!("{at}/{name}"));
                }
            }
            "allOf" | "anyOf" => {
                for (index, nested) in value.as_array().expect("schema list").iter().enumerate() {
                    assert_claude_accepts(nested, &format!("{at}/{index}"));
                }
            }
            "items" => assert_claude_accepts(value, &at),
            "additionalProperties" => assert_eq!(value, &json!(false), "{at}"),
            "format" => assert!(
                value
                    .as_str()
                    .is_some_and(|format| FORMATS.contains(&format)),
                "{at}: {value}"
            ),
            "minItems" => assert!(value.as_u64().is_some_and(|items| items <= 1), "{at}"),
            other => assert!(
                [
                    "$ref",
                    "const",
                    "default",
                    "description",
                    "enum",
                    "pattern",
                    "required",
                    "title",
                    "type"
                ]
                .contains(&other),
                "Claude structured outputs reject `{at}`"
            ),
        }
    }
}
