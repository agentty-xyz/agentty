//! Public span ownership and asynchronous context contract.

use std::future;
use std::sync::Arc;

use ag_telemetry::{
    Context, FutureExt as _, KeyValue, Outcome, QueuedTrace, Span, current_attribute, milestone,
};
use opentelemetry::global;
use opentelemetry::trace::{Status, TraceContextExt};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tokio::sync::Barrier;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spans_preserve_context_results_and_terminal_outcomes() {
    // Arrange
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider.clone());

    // Act
    let queued = QueuedTrace::new(
        "session.turn",
        vec![KeyValue::new("agentty.operation.id", "turn-1")],
    );
    queued.attribute("agentty.session.id", "session-1");
    let root = queued.selected();
    root.event("accepted");
    let root_context = root.context();
    let root_id = root_context.span().span_context().span_id();
    root.scope(async {
        current_attribute("agentty.turn.id", "turn-1");
        Span::root("child.session", Vec::new())
            .scope(async {})
            .await;
        let child = Span::child("utility.run");
        child.attribute("agentty.purpose", "title");
        child
            .scope(async {
                milestone("first_activity");
                let trace = Context::current();
                tokio::spawn(
                    async {
                        Span::child("agent.attempt")
                            .run(async { Ok::<_, ()>(42) }, |()| Outcome::Failed)
                            .await
                    }
                    .with_context(trace),
                )
                .await
                .expect("task")
                .expect("result");
            })
            .await;
        let failed = Span::child("failed")
            .run(async { Err::<(), _>("private error") }, |_| Outcome::Failed)
            .await;
        assert!(failed.is_err());
        let canceled = Span::child("canceled")
            .run(async { Err::<(), _>(()) }, |()| Outcome::Canceled)
            .await;
        assert!(canceled.is_err());
    })
    .await;
    async { Span::child("detached").scope(async {}).await }
        .with_context(root_context)
        .await;
    abandon_and_unwind_operations().await;
    interleave_operations().await;
    provider.force_flush().expect("flush");

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    let find = |name: &str| {
        spans
            .iter()
            .find(|span| span.name == name)
            .expect("named span")
    };
    assert_eq!(find("utility.run").parent_span_id, root_id);
    assert_eq!(
        find("child.session").links.links[0].span_context.span_id(),
        root_id
    );
    assert_ne!(
        find("detached").span_context.trace_id(),
        find("session.turn").span_context.trace_id()
    );
    assert_eq!(
        find("detached").links.links[0].span_context.span_id(),
        root_id
    );
    assert_eq!(
        find("agent.attempt").parent_span_id,
        find("utility.run").span_context.span_id()
    );
    assert_eq!(find("queue.wait").parent_span_id, root_id);
    assert!(matches!(find("failed").status, Status::Error { .. }));
    assert!(matches!(find("canceled").status, Status::Unset));
    for name in ["dropped", "abandoned", "canceled"] {
        assert!(
            find(name)
                .attributes
                .contains(&KeyValue::new("agentty.outcome", "canceled"))
        );
    }
    assert!(
        find("panic")
            .attributes
            .contains(&KeyValue::new("error.type", "panic"))
    );
    assert_eq!(find("utility.run").events.events[0].name, "first_activity");
    assert_eq!(find("session.turn").events.events[0].name, "accepted");
    assert!(!format!("{spans:?}").contains("private error"));
    assert!(!format!("{spans:?}").contains("private panic"));
    assert_context_ownership(&spans);
}

async fn abandon_and_unwind_operations() {
    let pending = Span::root("dropped", Vec::new());
    let task = tokio::spawn(pending.scope(future::pending::<()>()));
    task.abort();
    let _ = task.await;
    drop(QueuedTrace::new("abandoned", Vec::new()));
    let unwind = std::panic::catch_unwind(|| {
        let _span = Span::root("panic", Vec::new());
        std::panic::resume_unwind(Box::new("private panic"));
    });
    assert!(unwind.is_err());
}

async fn interleave_operations() {
    let barrier = Arc::new(Barrier::new(2));
    let tasks: Vec<_> = ["parallel.a", "parallel.b"]
        .into_iter()
        .map(|name| {
            let barrier = Arc::clone(&barrier);
            let span = Span::root(name, Vec::new());

            tokio::spawn(span.scope(async move {
                barrier.wait().await;
                Span::child(name).scope(tokio::task::yield_now()).await;
            }))
        })
        .collect();
    for task in tasks {
        let result = task.await;
        assert!(result.is_ok(), "{result:?}");
    }
}

fn assert_context_ownership(spans: &[SpanData]) {
    assert!(spans.iter().any(|span| {
        span.name == "session.turn"
            && span
                .attributes
                .contains(&KeyValue::new("agentty.turn.id", "turn-1"))
    }));
    for name in ["parallel.a", "parallel.b"] {
        let pair: Vec<_> = spans.iter().filter(|span| span.name == name).collect();
        assert_eq!(pair.len(), 2);
        assert_eq!(
            pair[0].span_context.trace_id(),
            pair[1].span_context.trace_id()
        );
        assert_eq!(pair[0].parent_span_id, pair[1].span_context.span_id());
    }
}
