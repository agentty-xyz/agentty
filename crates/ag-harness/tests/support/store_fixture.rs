//! Shared store, model, and input fixtures without test registrations.

#[path = "store.rs"]
mod backend;

use std::path::Path;
use std::sync::{Arc, Mutex};

use ag_harness::model::{ModelCompletion, ModelMessage, ModelRequest, ModelResponse};
use ag_harness::store::{
    AcquiredTurn, MemoryStore, NewSession, SessionStore, SqliteStore, WriteStatus,
};
use ag_harness::{
    Harness, ImageContent, ImageMediaType, InputBlock, Model, ModelError, OutputSchema,
    SessionError, ToolPolicy, TurnError, TurnInput, TurnOptions,
};
use async_trait::async_trait;
pub(crate) use backend::ExternalStore;
use serde_json::json;

use crate::context_budget_fixture::unbounded_context_budget;

pub(crate) fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false})).expect("schema")
}

pub(crate) fn options() -> TurnOptions {
    TurnOptions::new(schema(), ToolPolicy::default())
}

pub(crate) fn png_image(payload: &[u8]) -> ImageContent {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(payload);

    ImageContent::new(ImageMediaType::Png, bytes).expect("valid PNG image")
}

pub(crate) fn image_input(before: &str, payload: &[u8], after: &str) -> TurnInput {
    TurnInput::from_blocks(vec![
        InputBlock::Text(before.to_string()),
        InputBlock::Image(png_image(payload)),
        InputBlock::Text(after.to_string()),
    ])
    .expect("image input")
}

pub(crate) async fn stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(ExternalStore::new()),
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("sqlite"),
        ),
    ]
}

pub(crate) struct Echo {
    pub(crate) requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl Model for Echo {
    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let answer = request.prompt().to_string();
        self.requests.lock().expect("requests").push(request);

        Ok(ModelCompletion::from_response(ModelResponse::Output(
            json!({"answer":answer}),
        )))
    }
}

pub(crate) fn harness(store: Arc<dyn SessionStore>) -> Harness {
    Harness::new(
        Echo {
            requests: Arc::default(),
        },
        unbounded_context_budget(),
    )
    .store(store)
}

pub(crate) async fn lifecycle(store: Arc<dyn SessionStore>) {
    // Arrange
    let config = NewSession::new("session", schema())
        .with_optional_system_prompt(Some("policy".to_string()));
    store
        .create_session(&config, None, 256)
        .await
        .expect("create");
    let inputs: Vec<TurnInput> = ["first", "busy", "second"]
        .into_iter()
        .map(TurnInput::from)
        .collect();
    let acquired = AcquiredTurn::begin(Arc::clone(&store), "session", &inputs[0], &options(), 0)
        .await
        .expect("begin");
    let owner = acquired.owner().clone();

    // Act
    assert!(matches!(
        AcquiredTurn::begin(Arc::clone(&store), "session", &inputs[1], &options(), 0).await,
        Err(SessionError::Busy { .. })
    ));
    let write = store
        .write_intent(
            &owner,
            "call",
            Path::new("repository"),
            "file",
            Some(b"old"),
            b"new",
        )
        .await
        .expect("intent");
    store
        .complete_turn(&owner, &[ModelMessage::Assistant("answer".to_string())])
        .await
        .expect("complete");
    store
        .finish_write(&owner, write, true)
        .await
        .expect("settle after terminal");
    let successor = AcquiredTurn::begin(Arc::clone(&store), "session", &inputs[2], &options(), 0)
        .await
        .expect("successor");
    store.interrupt(&owner).await.expect("stale interrupt");

    // Assert
    assert!(store.renew(successor.owner()).await.is_ok());
    assert!(matches!(
        store.renew(&owner).await,
        Err(SessionError::OwnershipLost { .. })
    ));
    assert!(
        store
            .finish_write(successor.owner(), write, false)
            .await
            .is_err()
    );
    let records = store.load_writes("session").await.expect("writes");
    assert_eq!(records[0].status, WriteStatus::Applied);
    assert_eq!(
        records[0].expected_hash.as_deref(),
        Some("cba06b5736faf67e54b07b561eae94395e774c517a7d910a54369e1263ccfbd4")
    );
    assert_eq!(owner.session_id(), "session");
    assert_eq!(owner.store_identity(), store.identity());
    assert_eq!(owner.interruption_error_type(), "cancelled");
    let loaded = store.load_session("session").await.expect("load");
    assert_eq!(loaded.system_prompt.as_deref(), Some("policy"));
    assert_eq!(loaded.turns.len(), 1);
    store
        .fail_turn(
            successor.owner(),
            &TurnError::Model(ModelError::InvalidResponse),
        )
        .await
        .expect("fail");
    let failed = store.load_session("session").await.expect("failed history");
    assert_eq!(failed.turns.len(), 2);
    assert!(
        failed.turns[1]
            .stop
            .as_ref()
            .is_some_and(|stop| stop.failed)
    );
}
