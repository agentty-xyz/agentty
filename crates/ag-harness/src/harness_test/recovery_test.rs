use std::num::NonZeroUsize;
use std::sync::Arc;

use serde_json::json;

use crate::harness::Harness;
use crate::harness::tests::support::model;
use crate::repository::Repository;
use crate::{
    ExecutionIdentity, MemoryStore, OutputSchema, Tool, ToolPolicy, TurnLimits, TurnOptions,
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
        .host_request("id".into(), "input", &options)
        .expect("request");

    // Act / Assert
    let reordered = OutputSchema::new(serde_json::from_str(r#"{"properties":{"second":{"type":"string"},"first":{"type":"string"}},"type":"object"}"#).expect("json")).expect("schema");
    let reordered = TurnOptions::new(reordered, ToolPolicy::default(), TurnLimits::default());
    assert_eq!(
        request,
        session
            .host_request("id".into(), "input", &reordered)
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
                .host_request("id".into(), "input", &changed)
                .expect("changed")
                .fingerprint()
        );
    }
    session.system_prompt = Some("policy".into());
    assert_ne!(
        request,
        session
            .host_request("id".into(), "input", &options)
            .expect("system")
    );
    session.system_prompt = None;
    session.history.max_bytes = 1;
    assert_ne!(
        request,
        session
            .host_request("id".into(), "input", &options)
            .expect("budget")
    );
    session.history.max_bytes = 256 * 1024;
    session.harness.repository = Some(Repository::fixture("scope-one"));
    let scoped = session
        .host_request("id".into(), "input", &options)
        .expect("scope");
    assert_ne!(request, scoped);
    session.harness.repository = Some(Repository::fixture("scope-two"));
    assert_ne!(
        scoped,
        session
            .host_request("id".into(), "input", &options)
            .expect("scope")
    );
}
