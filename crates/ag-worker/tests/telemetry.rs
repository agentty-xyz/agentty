//! Trace inheritance through reusable and nested scoped utility clients.

use std::sync::Arc;

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::AgentResponse;
use ag_telemetry::{Span, TraceContextExt as _};
use ag_worker::{RunClient, RunScope, in_scope, scoped_client};
use async_trait::async_trait;
use opentelemetry::global;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

struct ObservedClient;

#[async_trait]
impl RunClient for ObservedClient {
    async fn submit(&self, _request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        Span::child("provider")
            .scope(async {
                Ok(OneShotSubmission {
                    response: AgentResponse::plain("done"),
                    stats: SessionStats::default(),
                })
            })
            .await
    }
}

#[tokio::test]
async fn nested_clients_inherit_active_traces_and_preserve_captured_traces() {
    // Arrange
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider);
    let client = scoped_client(Arc::new(ObservedClient), RunScope::default());
    let nested = scoped_client(client.clone(), RunScope::default());
    let turn = Span::root("session.turn", Vec::new());
    let turn_id = turn.context().span().span_context().span_id();

    // Act
    turn.scope(in_scope(RunScope::default(), async {
        nested.submit(request()).await.expect("nested submission");
        let captured = scoped_client(client, RunScope::default());
        let other = Span::root("other.turn", Vec::new());
        other
            .scope(in_scope(RunScope::default(), async {
                tokio::spawn(async move { captured.submit(request()).await })
                    .await
                    .expect("spawned submission")
                    .expect("captured submission");
            }))
            .await;
    }))
    .await;
    nested.submit(request()).await.expect("untraced submission");

    // Assert
    let spans = exporter.get_finished_spans().expect("finished spans");
    let calls: Vec<_> = spans
        .iter()
        .filter(|span| span.name == "provider")
        .collect();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].parent_span_id, turn_id);
    assert_eq!(calls[1].parent_span_id, turn_id);
    assert_eq!(calls[2].parent_span_id, SpanId::INVALID);
}

fn request() -> OneShotRequest {
    OneShotRequest {
        execution_policy: ag_contracts::ExecutionPolicy::default(),
        child_pid: None,
        folder: "repository".into(),
        harness: "codex".into(),
        model: "model".into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "title".into(),
        provider_call_budget: None,
        reasoning_level: ReasoningLevel::default(),
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::Normal,
    }
}
