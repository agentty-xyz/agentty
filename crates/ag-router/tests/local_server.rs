//! Local model server discovery against recorded Ollama and `OpenAI` API
//! shapes.

use ag_router::{LocalInventory, LocalModel, LocalServer, LocalServerError, LocalServerKind};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Returns a client for `server`'s `/v1` endpoint.
fn local_server(server: &MockServer) -> LocalServer {
    LocalServer::new(format!("{}/v1/", server.uri()), None)
}

/// Mounts `GET {route}` answering `body`.
async fn mount_get(server: &MockServer, route: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn ollama_inventory_reads_tags_capabilities_and_loaded_windows() {
    // Arrange
    let server = MockServer::start().await;
    mount_get(
        &server,
        "/api/tags",
        json!({"models": [
            {
                "name": "qwen3:latest",
                "details": {"context_length": 40960},
                "capabilities": ["completion", "tools", "thinking"]
            },
            {
                "name": "gemma:latest",
                "details": {"context_length": 8192},
                "capabilities": ["completion"]
            },
            {
                "name": "nomic-embed-text:latest",
                "details": {"context_length": 2048},
                "capabilities": ["embedding"]
            }
        ]}),
    )
    .await;
    mount_get(
        &server,
        "/api/ps",
        json!({"models": [{"name": "qwen3:latest", "context_length": 4096}]}),
    )
    .await;

    // Act
    let inventory = local_server(&server).inventory().await;

    // Assert
    assert_eq!(
        inventory.expect("Ollama answers"),
        LocalInventory {
            kind: LocalServerKind::Ollama,
            models: vec![
                LocalModel {
                    context_window: Some(40_960),
                    id: "qwen3:latest".to_string(),
                    loaded_context_window: Some(4096),
                    supports_tools: Some(true),
                },
                LocalModel {
                    context_window: Some(8192),
                    id: "gemma:latest".to_string(),
                    loaded_context_window: None,
                    supports_tools: Some(false),
                },
            ],
        }
    );
}

#[tokio::test]
async fn older_ollama_tags_are_completed_from_show() {
    // Arrange
    let server = MockServer::start().await;
    mount_get(
        &server,
        "/api/tags",
        json!({"models": [{"name": "llama3.2:3b", "details": {}}, {"name": "broken:latest"}]}),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json(json!({"model": "llama3.2:3b"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "capabilities": ["completion", "tools"],
            "model_info": {"general.architecture": "llama", "llama.context_length": 131_072}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_json(json!({"model": "broken:latest"})))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    // Act
    let inventory = local_server(&server)
        .inventory()
        .await
        .expect("Ollama answers");

    // Assert
    assert_eq!(inventory.kind, LocalServerKind::Ollama);
    assert_eq!(
        inventory.models,
        vec![
            LocalModel {
                context_window: Some(131_072),
                id: "llama3.2:3b".to_string(),
                loaded_context_window: None,
                supports_tools: Some(true),
            },
            LocalModel {
                context_window: None,
                id: "broken:latest".to_string(),
                loaded_context_window: None,
                supports_tools: None,
            },
        ]
    );
}

#[tokio::test]
async fn other_servers_are_listed_through_the_models_api_with_credentials() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(bearer_token("local-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                {"id": "Qwen/Qwen3-8B", "object": "model", "max_model_len": 32768},
                {"id": "local-model", "object": "model"}
            ]
        })))
        .mount(&server)
        .await;
    let local_server = LocalServer::new(format!("{}/v1", server.uri()), Some("local-key".into()));

    // Act
    let inventory = local_server.inventory().await.expect("models API answers");

    // Assert
    assert_eq!(inventory.kind, LocalServerKind::OpenAiCompatible);
    assert_eq!(
        LocalServerKind::OpenAiCompatible.display_name(),
        "OpenAI-compatible server"
    );
    assert_eq!(LocalServerKind::Ollama.display_name(), "Ollama");
    assert_eq!(
        inventory.models,
        vec![
            LocalModel {
                context_window: Some(32_768),
                id: "Qwen/Qwen3-8B".to_string(),
                loaded_context_window: Some(32_768),
                supports_tools: None,
            },
            LocalModel {
                context_window: None,
                id: "local-model".to_string(),
                loaded_context_window: None,
                supports_tools: None,
            },
        ]
    );
}

#[tokio::test]
async fn an_unreachable_server_reports_an_error() {
    // Arrange
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("free port");
    let base_url = format!("http://{}/v1", listener.local_addr().expect("address"));
    drop(listener);

    // Act
    let inventory = LocalServer::new(base_url, Some(" ".into()))
        .inventory()
        .await;

    // Assert
    assert!(matches!(inventory, Err(LocalServerError::Request { .. })));
}

#[tokio::test]
async fn a_failing_models_api_reports_its_status() {
    // Arrange
    let server = MockServer::start().await;

    // Act
    let error = local_server(&server)
        .inventory()
        .await
        .expect_err("no API answers");

    // Assert
    assert!(matches!(
        error,
        LocalServerError::Status { status: 404, .. }
    ));
    assert!(error.to_string().ends_with("/v1/models returned HTTP 404"));
}

#[tokio::test]
async fn running_window_of_a_loaded_ollama_model_comes_from_ps() {
    // Arrange
    let server = MockServer::start().await;
    mount_get(
        &server,
        "/api/ps",
        json!({"models": [{"name": "qwen3:latest", "context_length": 32768}]}),
    )
    .await;

    // Act
    let window = local_server(&server)
        .running_context_window("qwen3:latest")
        .await;

    // Assert
    assert_eq!(window.expect("Ollama answers"), Some(32_768));
}

#[tokio::test]
async fn running_window_loads_an_unloaded_ollama_model_first() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .and(body_json(json!({"model": "qwen3:latest"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "qwen3:latest", "response": "", "done": true, "done_reason": "load"
        })))
        .expect(1)
        .mount(&server)
        .await;
    mount_get(
        &server,
        "/api/ps",
        json!({"models": [{"name": "qwen3:latest", "context_length": 4096}]}),
    )
    .await;

    // Act
    let window = local_server(&server)
        .running_context_window("qwen3:latest")
        .await;

    // Assert
    assert_eq!(window.expect("Ollama answers"), Some(4096));
}

#[tokio::test]
async fn running_window_of_other_servers_comes_from_the_models_api() {
    // Arrange
    let server = MockServer::start().await;
    mount_get(
        &server,
        "/v1/models",
        json!({"data": [{"id": "Qwen/Qwen3-8B", "max_model_len": 16384}]}),
    )
    .await;

    // Act
    let known = local_server(&server)
        .running_context_window("Qwen/Qwen3-8B")
        .await;
    let unknown = local_server(&server).running_context_window("other").await;

    // Assert
    assert_eq!(known.expect("models API answers"), Some(16_384));
    assert_eq!(unknown.expect("models API answers"), None);
}

#[tokio::test]
async fn running_window_reports_a_failed_ollama_load() {
    // Arrange
    let server = MockServer::start().await;
    mount_get(&server, "/api/ps", json!({"models": []})).await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    // Act
    let window = local_server(&server)
        .running_context_window("missing")
        .await;

    // Assert
    assert!(matches!(
        window,
        Err(LocalServerError::Status { status: 404, .. })
    ));
}
