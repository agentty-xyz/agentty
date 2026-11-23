//! Local model server discovery against recorded Ollama and `OpenAI` API
//! shapes.

use ag_router::{LocalInventory, LocalServer, LocalServerError, LocalServerKind};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A model's context window, identifier, loaded context window, and tool
/// support.
type ModelFields<'a> = (Option<u64>, &'a str, Option<u64>, Option<bool>);

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
    let inventory = inventory.expect("Ollama answers");
    assert_eq!(inventory.kind, LocalServerKind::Ollama);
    assert_eq!(
        model_fields(&inventory),
        [
            (Some(40_960), "qwen3:latest", Some(4096), Some(true)),
            (Some(8192), "gemma:latest", None, Some(false)),
        ]
    );
}

/// Returns each model's fields in the inventory's order.
fn model_fields(inventory: &LocalInventory) -> Vec<ModelFields<'_>> {
    inventory
        .models
        .iter()
        .map(|model| {
            (
                model.context_window,
                model.id.as_str(),
                model.loaded_context_window,
                model.supports_tools,
            )
        })
        .collect()
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
        model_fields(&inventory),
        [
            (Some(131_072), "llama3.2:3b", None, Some(true)),
            (None, "broken:latest", None, None),
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
        model_fields(&inventory),
        [
            (Some(32_768), "Qwen/Qwen3-8B", Some(32_768), None),
            (None, "local-model", None, None),
        ]
    );
}

#[tokio::test]
async fn an_unreachable_server_reports_an_error() {
    // Arrange: nothing can listen on port 0, whereas a port freed after
    // binding can be taken by another test's server before the request.
    let base_url = "http://127.0.0.1:0/v1";

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
