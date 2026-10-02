//! Trace inheritance through reusable and nested scoped utility clients.

use std::sync::{Arc, Mutex};

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotRequest, OneShotSubmission, PermissionMode,
    ReasoningLevel, SessionStats, SpeedMode,
};
use ag_protocol::AgentResponse;
use ag_telemetry::{CaptureToolContent, Context, FutureExt as _, Span, TraceContextExt as _};
use ag_worker::{RunClient, RunScope, in_scope, scoped_client};
use async_trait::async_trait;
use opentelemetry::global;
use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
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

struct ContextClient {
    observed: Arc<Mutex<Option<Context>>>,
}

#[async_trait]
impl RunClient for ContextClient {
    async fn submit(&self, _request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        *self
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Context::current());

        Ok(OneShotSubmission {
            response: AgentResponse::plain("done"),
            stats: SessionStats::default(),
        })
    }
}

#[tokio::test]
async fn scoped_clients_preserve_capture_policy_independently_of_span_inheritance() {
    // Arrange / Act / Assert
    for captured_span in [None, Some(1)] {
        for submitting_span in [None, Some(2)] {
            for captured_policy in [None, Some(false), Some(true)] {
                for submitting_policy in [None, Some(false), Some(true)] {
                    let observed = Arc::new(Mutex::new(None));
                    let client = async {
                        scoped_client(
                            Arc::new(ContextClient {
                                observed: observed.clone(),
                            }),
                            RunScope::default(),
                        )
                    }
                    .with_context(trace_context(captured_policy, captured_span))
                    .await;
                    let mut review = request();
                    review.request_kind = AgentRequestKind::FocusedReview;

                    tokio::spawn(async move {
                        client
                            .submit(review)
                            .with_context(trace_context(submitting_policy, submitting_span))
                            .await
                    })
                    .await
                    .expect("spawned focused review")
                    .expect("focused review submission");

                    let context = observed.lock().expect("observed context");
                    let context = context.as_ref().expect("submission observed");
                    assert_eq!(
                        context.get::<CaptureToolContent>().map(|policy| policy.0),
                        captured_policy.or(submitting_policy),
                        "captured={captured_policy:?}, submitting={submitting_policy:?}"
                    );
                    assert_eq!(
                        context.span().span_context().span_id(),
                        captured_span
                            .or(submitting_span)
                            .map_or(SpanId::INVALID, SpanId::from)
                    );
                }
            }
        }
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

fn trace_context(policy: Option<bool>, span_id: Option<u64>) -> Context {
    let mut context = Context::new();
    if let Some(span_id) = span_id {
        context = context.with_remote_span_context(SpanContext::new(
            TraceId::from(123),
            SpanId::from(span_id),
            TraceFlags::default(),
            true,
            TraceState::default(),
        ));
    }
    if let Some(policy) = policy {
        context = context.with_value(CaptureToolContent(policy));
    }

    context
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
