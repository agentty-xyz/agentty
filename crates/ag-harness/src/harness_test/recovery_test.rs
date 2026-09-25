use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::json;

use crate::harness::Harness;
use crate::harness::tests::support::{model, response_without_metadata};
use crate::model::{MockModel, ModelCapabilities, ModelError, ModelRegistry, ModelResponse};
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::store::MemoryStore;
use crate::store_conformance_test::{image_input, png_image};
use crate::{
    InputBlock, OutputSchema, SessionError, Tool, ToolPolicy, TurnError, TurnInput, TurnLimits,
    TurnOptions,
};

#[tokio::test]
async fn recovery_fingerprint_covers_effective_configuration_and_canonicalizes_schema() {
    // Arrange
    let harness = Harness::new(model())
        .store(Arc::new(MemoryStore::new()))
        .execution_identity(ExecutionIdentity::new("injected", "v1").expect("identity"));
    let schema = OutputSchema::new(json!({"type":"object","properties":{"first":{"type":"string"},"second":{"type":"string"}}})).expect("schema");
    let options = TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
    let mut session = harness
        .session("session", schema)
        .create()
        .await
        .expect("session");
    let request = session
        .host_request("id".into(), &TurnInput::from("input"), &options)
        .expect("request");

    // Act / Assert
    let reordered = OutputSchema::new(serde_json::from_str(r#"{"properties":{"second":{"type":"string"},"first":{"type":"string"}},"type":"object"}"#).expect("json")).expect("schema");
    let reordered = TurnOptions::new(reordered, ToolPolicy::default(), TurnLimits::default());
    assert_eq!(
        request,
        session
            .host_request("id".into(), &TurnInput::from("input"), &reordered)
            .expect("canonical")
    );
    for changed in [
        TurnOptions::new(
            OutputSchema::new(json!({"type":"object"})).expect("schema"),
            ToolPolicy::default(),
            TurnLimits::default(),
        ),
        TurnOptions::new(
            options.schema().clone(),
            ToolPolicy::default().allow(Tool::Read),
            TurnLimits::default(),
        ),
        TurnOptions::new(
            options.schema().clone(),
            ToolPolicy::default(),
            TurnLimits::new(NonZeroUsize::MIN),
        ),
    ] {
        assert_ne!(
            request.fingerprint(),
            session
                .host_request("id".into(), &TurnInput::from("input"), &changed)
                .expect("changed")
                .fingerprint()
        );
    }
    session.system_prompt = Some("policy".into());
    assert_ne!(
        request,
        session
            .host_request("id".into(), &TurnInput::from("input"), &options)
            .expect("system")
    );
    session.system_prompt = None;
    session.history.max_bytes = 1;
    assert_ne!(
        request,
        session
            .host_request("id".into(), &TurnInput::from("input"), &options)
            .expect("budget")
    );
    session.history.max_bytes = 256 * 1024;
    session.harness.repository = Some(Repository::fixture("scope-one"));
    let scoped = session
        .host_request("id".into(), &TurnInput::from("input"), &options)
        .expect("scope");
    assert_ne!(request, scoped);
    session.harness.repository = Some(Repository::fixture("scope-two"));
    assert_ne!(
        scoped,
        session
            .host_request("id".into(), &TurnInput::from("input"), &options)
            .expect("scope")
    );
}

#[tokio::test]
async fn image_fingerprints_track_content_and_image_capability_only() {
    // Arrange
    let registration = |image_capable: bool| {
        let mut registry = ModelRegistry::new();
        registry
            .register(
                ExecutionIdentity::new("registered", "1").expect("identity"),
                model(),
                ModelCapabilities {
                    context_budget: None,
                    image_input: image_capable,
                    native_continuation: false,
                    tool_calls: false,
                },
            )
            .expect("register");

        registry.resolve("registered").expect("resolve").clone()
    };
    let harness = Harness::new(model())
        .store(Arc::new(MemoryStore::new()))
        .execution_identity(ExecutionIdentity::new("injected", "v1").expect("identity"));
    let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
    let options = TurnOptions::new(schema.clone(), ToolPolicy::default(), TurnLimits::default());
    let mut session = harness
        .session("session", schema)
        .create()
        .await
        .expect("session");
    let text = TurnInput::from("look");
    let image = image_input("look", b"one", "closely");
    let changed_image = image_input("look", b"two", "closely");
    let reordered = TurnInput::from_blocks({
        let mut blocks = image.blocks().to_vec();
        blocks.swap(0, 2);

        blocks
    })
    .expect("reordered input");
    let extra_image = TurnInput::from_blocks({
        let mut blocks = image.blocks().to_vec();
        blocks.push(InputBlock::Image(png_image(b"one")));

        blocks
    })
    .expect("extended input");

    // Act
    let text_request = session
        .host_request("id".into(), &text, &options)
        .expect("text request");
    let image_request = session
        .host_request("id".into(), &image, &options)
        .expect("image request");

    // Assert
    for different in [&changed_image, &reordered, &extra_image] {
        assert_ne!(
            image_request.fingerprint(),
            session
                .host_request("id".into(), different, &options)
                .expect("different content")
                .fingerprint()
        );
    }
    assert_ne!(text_request.fingerprint(), image_request.fingerprint());
    assert_eq!(
        image_request,
        session
            .host_request(
                "id".into(),
                &image_input("look", b"one", "closely"),
                &options
            )
            .expect("repeatable")
    );
    session.harness.model_registration = Some(registration(false));
    let text_without_capability = session
        .host_request("id".into(), &text, &options)
        .expect("text without capability");
    let image_without_capability = session
        .host_request("id".into(), &image, &options)
        .expect("image without capability");
    session.harness.model_registration = Some(registration(true));
    assert_eq!(
        text_without_capability,
        session
            .host_request("id".into(), &text, &options)
            .expect("text with capability"),
        "image capability changes must not invalidate recorded text retries"
    );
    assert_ne!(
        image_without_capability.fingerprint(),
        session
            .host_request("id".into(), &image, &options)
            .expect("image with capability")
            .fingerprint()
    );
}

#[tokio::test]
async fn registered_capability_gates_image_input_before_execution() {
    // Arrange
    let harness = |image_capable: bool, model: MockModel| {
        let mut registry = ModelRegistry::new();
        registry
            .register(
                ExecutionIdentity::new("registered", "1").expect("identity"),
                model,
                ModelCapabilities {
                    context_budget: None,
                    image_input: image_capable,
                    native_continuation: false,
                    tool_calls: false,
                },
            )
            .expect("register");

        Harness::from_registry(&registry, "registered")
            .expect("harness")
            .store(Arc::new(MemoryStore::new()))
    };
    let schema = OutputSchema::new(json!({"type":"object"})).expect("schema");
    let input = || image_input("look", b"one", "closely");
    let rejecting = harness(false, model());
    let mut capable_model = model();
    capable_model
        .expect_complete()
        .times(1)
        .returning(|_| Ok(response_without_metadata(ModelResponse::Output(json!({})))));
    let capable = harness(true, capable_model);

    // Act
    let once = rejecting.run_once(input(), schema.clone()).await;
    let mut session = rejecting
        .session("no-images", schema.clone())
        .create()
        .await
        .expect("session");
    let durable = session.send(input()).await;
    let accepted = capable.run_once(input(), schema).await;

    // Assert
    assert!(matches!(
        once,
        Err(TurnError::Model(ModelError::UnsupportedImageInput { .. }))
    ));
    assert!(matches!(
        durable,
        Err(SessionError::Turn(TurnError::Model(
            ModelError::UnsupportedImageInput { .. }
        )))
    ));
    assert_eq!(accepted.expect("image-capable turn").output(), &json!({}));
}

#[tokio::test]
async fn image_input_beyond_the_history_budget_fails_before_execution() {
    // Arrange
    let harness = Harness::new(model())
        .store(Arc::new(MemoryStore::new()))
        .max_history_bytes(NonZeroUsize::new(64).expect("budget"));
    let mut session = harness
        .session(
            "budget",
            OutputSchema::new(json!({"type":"object"})).expect("schema"),
        )
        .create()
        .await
        .expect("session");

    // Act
    let rejected = session.send(image_input("look", &[0; 64], "closely")).await;

    // Assert
    assert!(matches!(
        rejected,
        Err(SessionError::ImageInputExceedsHistory {
            max_history_bytes: 64,
            ..
        })
    ));
}
