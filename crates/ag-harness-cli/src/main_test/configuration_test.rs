use std::env;
use std::path::PathBuf;

use ag_harness::{ModelConfigurationError, ModelProvider};
use serde_json::json;

use super::support::{parse_cli, resume_arguments, run_arguments};
use crate::{ChatMode, CliError, chat_schema, database_path, stored_model_identity_parts};

#[test]
fn cli_accepts_chat_with_or_without_an_initial_prompt() {
    // Arrange and Act
    let without_prompt =
        parse_cli(["ag-harness", "run", "muse-custom"]).expect("chat arguments should parse");
    let with_prompt = parse_cli([
        "ag-harness",
        "run",
        "muse-custom",
        "Summarize this change",
        "--provider",
        "qwen",
        "--base-url",
        "https://models.example/v1",
        "--read-dir",
        "repo",
        "--allow-write",
    ])
    .expect("an initial prompt should parse");
    let blank_prompt = parse_cli(["ag-harness", "run", "muse-custom", "  "])
        .expect_err("a blank initial prompt should be rejected");
    let unknown_provider = parse_cli(["ag-harness", "run", "muse-custom", "--provider", "unknown"])
        .expect_err("an unknown provider should be rejected");

    // Assert
    let without_prompt =
        run_arguments(without_prompt.command).expect("run command should contain run arguments");
    assert_eq!(without_prompt.prompt, None);
    assert!(!without_prompt.allow_write);
    assert_eq!(without_prompt.provider, ModelProvider::Muse);
    assert_eq!(without_prompt.read_dir, PathBuf::from("."));
    let with_prompt =
        run_arguments(with_prompt.command).expect("run command should contain run arguments");
    assert_eq!(with_prompt.model, "muse-custom");
    assert_eq!(with_prompt.prompt.as_deref(), Some("Summarize this change"));
    assert_eq!(with_prompt.provider, ModelProvider::Qwen);
    assert_eq!(
        with_prompt.base_url.as_deref(),
        Some("https://models.example/v1")
    );
    assert_eq!(with_prompt.read_dir, PathBuf::from("repo"));
    assert!(with_prompt.allow_write);
    assert!(
        blank_prompt
            .to_string()
            .contains("prompt must contain a non-whitespace character")
    );
    assert!(
        unknown_provider
            .to_string()
            .contains("invalid value 'unknown'")
    );
}

#[test]
fn cli_accepts_every_catalog_provider() {
    // Arrange and Act
    let providers = ModelProvider::all()
        .iter()
        .map(|provider| {
            parse_cli([
                "ag-harness",
                "run",
                "model-id",
                "--provider",
                provider.as_str(),
            ])
            .expect("catalog provider should parse")
        })
        .collect::<Vec<_>>();

    // Assert
    for (cli, expected) in providers.into_iter().zip(ModelProvider::all()) {
        let args = run_arguments(cli.command).expect("run command should contain arguments");
        assert_eq!(args.provider, *expected);
    }
}

#[test]
fn cli_accepts_resume_and_database_override() {
    // Arrange and Act
    let cli = parse_cli([
        "ag-harness",
        "--database",
        "state.db",
        "resume",
        "session-a",
        "continue",
        "--allow-write",
    ])
    .expect("resume arguments should parse");

    // Assert
    assert_eq!(cli.database, Some(PathBuf::from("state.db")));
    let args = resume_arguments(cli.command).expect("resume command should contain arguments");
    assert_eq!(args.session, "session-a");
    assert_eq!(args.prompt.as_deref(), Some("continue"));
    assert!(args.allow_write);

    let resume_probe =
        parse_cli(["ag-harness", "resume", "session-b"]).expect("resume arguments should parse");
    let run_probe = parse_cli(["ag-harness", "run", "model"]).expect("run arguments should parse");
    assert!(run_arguments(resume_probe.command).is_none());
    assert!(resume_arguments(run_probe.command).is_none());
}

#[test]
fn database_path_uses_explicit_root_or_home_and_rejects_missing_storage_root() {
    // Arrange and Act
    let explicit_environment_calls = std::cell::Cell::new(0);
    let mut explicit_environment = |_: &str| {
        explicit_environment_calls.set(explicit_environment_calls.get() + 1);

        Err(env::VarError::NotPresent)
    };
    let explicit = database_path(
        Some(PathBuf::from("explicit.db")),
        &mut explicit_environment,
    );
    let rooted_variables = std::collections::HashMap::from([("AG_HARNESS_ROOT", "/state/harness")]);
    let rooted = database_path(None, &mut |name| {
        rooted_variables
            .get(name)
            .map(ToString::to_string)
            .ok_or(env::VarError::NotPresent)
    });
    let home_variables = std::collections::HashMap::from([("HOME", "/home/user")]);
    let home = database_path(None, &mut |name| {
        home_variables
            .get(name)
            .map(ToString::to_string)
            .ok_or(env::VarError::NotPresent)
    });
    let missing = database_path(None, &mut |_| Err(env::VarError::NotPresent));
    let empty_home = database_path(None, &mut |name| match name {
        "HOME" => Ok(String::new()),
        _ => Err(env::VarError::NotPresent),
    });

    // Assert
    assert_eq!(
        explicit.expect("explicit database should resolve"),
        PathBuf::from("explicit.db")
    );
    assert_eq!(explicit_environment_calls.get(), 0);
    assert!(explicit_environment("unused").is_err());
    assert_eq!(explicit_environment_calls.get(), 1);
    assert_eq!(
        rooted.expect("rooted database should resolve"),
        PathBuf::from("/state/harness/db/harness.db")
    );
    assert_eq!(
        home.expect("home database should resolve"),
        PathBuf::from("/home/user/.ag-harness/db/harness.db")
    );
    assert!(matches!(missing, Err(CliError::DatabaseLocation)));
    assert!(matches!(empty_home, Err(CliError::DatabaseLocation)));
}

#[test]
fn chat_mode_accounts_for_both_terminal_streams_and_initial_prompt() {
    // Arrange
    let with_prompt =
        parse_cli(["ag-harness", "run", "muse", "hello"]).expect("chat arguments should parse");
    let without_prompt =
        parse_cli(["ag-harness", "run", "muse"]).expect("chat arguments should parse");
    let resume = parse_cli(["ag-harness", "resume", "session-a", "continue"])
        .expect("resume arguments should parse");

    // Act and Assert
    assert_eq!(
        ChatMode::detect(&with_prompt, true, true),
        ChatMode::Interactive
    );
    assert_eq!(
        ChatMode::detect(&with_prompt, true, false),
        ChatMode::OneShot
    );
    assert_eq!(
        ChatMode::detect(&with_prompt, false, false),
        ChatMode::NonInteractive
    );
    assert_eq!(
        ChatMode::detect(&without_prompt, true, false),
        ChatMode::NonInteractive
    );
    assert_eq!(ChatMode::detect(&resume, true, false), ChatMode::OneShot);
}

#[test]
fn stored_model_identity_maps_supported_providers_and_rejects_incomplete_identity() {
    // Arrange and Act
    let muse = stored_model_identity_parts(Some("meta"), Some("muse-model"));
    let kimi = stored_model_identity_parts(Some("moonshot_ai"), Some("kimi-model"));
    let qwen = stored_model_identity_parts(Some("alibaba_cloud"), Some("qwen-model"));
    let unknown = stored_model_identity_parts(Some("unknown"), Some("model"));
    let missing_model = stored_model_identity_parts(Some("meta"), None);

    // Assert
    assert!(matches!(muse, Ok((ModelProvider::Muse, model)) if model == "muse-model"));
    assert!(matches!(kimi, Ok((ModelProvider::Kimi, model)) if model == "kimi-model"));
    assert!(matches!(qwen, Ok((ModelProvider::Qwen, model)) if model == "qwen-model"));
    assert!(matches!(unknown, Err(CliError::MissingModelIdentity)));
    assert!(matches!(missing_model, Err(CliError::MissingModelIdentity)));
}

#[test]
fn cli_configuration_errors_preserve_cli_specific_guidance() {
    // Arrange
    let base_url = ModelConfigurationError::BaseUrl {
        name: "KIMI_BASE_URL",
    };
    let api_key = ModelConfigurationError::ApiKey {
        name: "MODEL_API_KEY",
    };

    // Act
    let base_url = CliError::from(base_url);
    let api_key = CliError::from(api_key);

    // Assert
    assert_eq!(
        base_url.to_string(),
        "--base-url or KIMI_BASE_URL is required"
    );
    assert_eq!(api_key.to_string(), "MODEL_API_KEY is unavailable");
}

#[test]
fn chat_schema_requires_one_message_string() {
    // Arrange and Act
    let schema = chat_schema().expect("chat schema should compile");

    // Assert
    assert_eq!(schema.value()["required"], json!(["message"]));
    assert_eq!(schema.value()["additionalProperties"], json!(false));
}
