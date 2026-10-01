use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ag_contracts::{
    AgentRequestKind, OneShotClient as _, OneShotRequest, PermissionMode, ReasoningLevel, SpeedMode,
};
use ag_protocol::ProtocolSchemaInstructionMode;
use ag_telemetry::{Span, TraceContextExt as _};
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tokio::sync::mpsc;

use crate::agent::submission::RealOneShotClient;
use crate::app_server::{
    AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse, RuntimeInspector,
    run_turn_with_restart_retry,
};
use crate::telemetry::TRACER_PROVIDER_LOCK;

struct ObservedServer {
    malformed: AtomicBool,
    sessions: Arc<AppServerSessionRegistry<()>>,
}

impl AppServerClient for ObservedServer {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        _stream: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let sessions = Arc::clone(&self.sessions);
        let malformed = self.malformed.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            run_turn_with_restart_retry(
                &sessions,
                request,
                RuntimeInspector {
                    matches_request: |(), _| true,
                    pid: |()| None,
                    provider_conversation_id: |()| None,
                    restored_context: |()| false,
                    retain_runtime_after_turn: false,
                },
                ProtocolSchemaInstructionMode::PromptSchema,
                |_| Box::pin(async { Ok(()) }),
                move |(), _| {
                    Box::pin(async move {
                        let answer = if malformed {
                            "invalid JSON"
                        } else {
                            r#"{"answer":"done"}"#
                        };
                        Ok((answer.to_string(), 1, 1))
                    })
                },
                |()| Box::pin(async {}),
            )
            .await
        })
    }

    fn shutdown_session(&self, _session_id: String) -> AppServerFuture<()> {
        Box::pin(async { Span::child("utility.cleanup").scope(async {}).await })
    }
}

#[tokio::test]
async fn owned_submission_tasks_preserve_utility_parents_through_repair_and_cleanup() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider);
    for pooled in [false, true] {
        for repair in [false, true] {
            exporter.reset();
            let server = Arc::new(ObservedServer {
                malformed: AtomicBool::new(repair),
                sessions: Arc::new(AppServerSessionRegistry::new("trace test")),
            });
            let client = if pooled {
                RealOneShotClient::pooled(Some(server))
            } else {
                RealOneShotClient::new(Some(server))
            };

            // Act
            let utility = Span::root("utility.run", Vec::new());
            let trace_id = utility.context().span().span_context().trace_id();
            utility
                .scope(async {
                    client.submit(request()).await.expect("utility result");
                    client.close().await;
                })
                .await;

            // Assert
            let spans: Vec<_> = exporter
                .get_finished_spans()
                .expect("finished spans")
                .into_iter()
                .filter(|span| span.span_context.trace_id() == trace_id)
                .collect();
            let root = spans
                .iter()
                .find(|span| span.name == "utility.run")
                .expect("utility root");
            for name in ["agent.startup", "agent.attempt", "utility.cleanup"] {
                let children: Vec<_> = spans.iter().filter(|span| span.name == name).collect();
                assert!(!children.is_empty(), "{name}");
                for child in children {
                    assert_eq!(child.parent_span_id, root.span_context.span_id(), "{name}");
                    assert_eq!(
                        child.span_context.trace_id(),
                        root.span_context.trace_id(),
                        "{name}"
                    );
                }
            }
            assert_eq!(
                spans
                    .iter()
                    .filter(|span| span.name == "agent.attempt")
                    .count(),
                if repair { 2 } else { 1 }
            );
        }
    }
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
