//! Public span ownership and asynchronous context contract.

use std::future;
use std::sync::Arc;
use std::time::SystemTime;

use ag_telemetry::{
    CaptureToolContent, Context, FutureExt as _, KeyValue, Outcome, QueuedTrace, Span,
    current_attribute, milestone,
};
use opentelemetry::global;
use opentelemetry::trace::{
    SpanContext, SpanId, Status, TraceContextExt, TraceFlags, TraceId, TraceState, Tracer,
};
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider, SpanData};
use tokio::sync::{Barrier, Mutex};

static PROVIDER_LOCK: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn capture_policy_follows_roots_queues_tasks_and_detached_spans() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter)
            .build(),
    );

    // Act / Assert
    for enabled in [false, true] {
        async {
            let queued = QueuedTrace::new("queue", Vec::new());
            let root = queued.selected();
            let context = root.context();
            root.scope(async {
                assert_eq!(
                    Context::current()
                        .get::<CaptureToolContent>()
                        .expect("policy")
                        .0,
                    enabled
                );
                let nested = Span::root("independent", Vec::new());
                assert_eq!(
                    nested
                        .context()
                        .get::<CaptureToolContent>()
                        .expect("policy")
                        .0,
                    enabled
                );
                tokio::spawn(nested.scope(async move {
                    assert_eq!(
                        Context::current()
                            .get::<CaptureToolContent>()
                            .expect("policy")
                            .0,
                        enabled
                    );
                }))
                .await
                .expect("task");
            })
            .await;
            async {
                let detached = Span::child("detached");
                assert_eq!(
                    detached
                        .context()
                        .get::<CaptureToolContent>()
                        .expect("policy")
                        .0,
                    enabled
                );
                detached.finish(Outcome::Completed);
            }
            .with_context(context)
            .await;
        }
        .with_context(Context::new().with_value(CaptureToolContent(enabled)))
        .await;
    }
    assert!(
        !Span::root("default", Vec::new())
            .context()
            .get::<CaptureToolContent>()
            .expect("default policy")
            .0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spans_preserve_context_results_and_terminal_outcomes() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;
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
        Span::child_at("reported.interval", SystemTime::UNIX_EPOCH)
            .scope(async {})
            .await;
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

#[tokio::test]
async fn live_unsampled_parents_preserve_sampling_and_finished_owned_parents_detach() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::AlwaysOn)))
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let remote = SpanContext::new(
        TraceId::from(123),
        SpanId::from(456),
        TraceFlags::default(),
        true,
        TraceState::default(),
    );
    let external = Context::new().with_remote_span_context(remote.clone());

    // Act
    let owned_context = async {
        let child = Span::child("unsampled.child");
        let context = child.context();
        child
            .scope(async {
                let nested = Span::child("unsampled.nested");
                assert_eq!(
                    nested.context().span().span_context().trace_id(),
                    remote.trace_id()
                );
                assert!(!nested.context().span().is_recording());
                nested.finish(Outcome::Completed);
            })
            .await;

        context
    }
    .with_context(external)
    .await;

    // Assert
    assert_eq!(
        owned_context.span().span_context().trace_id(),
        remote.trace_id()
    );
    assert!(!owned_context.span().span_context().is_sampled());
    assert_eq!(exporter.get_finished_spans().expect("spans"), []);

    // Act
    async { Span::child("detached.unsampled").scope(async {}).await }
        .with_context(owned_context.clone())
        .await;
    // Replacing the span must not inherit the old owned span's completion.
    let tracer = global::tracer("host");
    let host_span = tracer.start("host.live");
    let host_context = owned_context.with_span(host_span);
    async { Span::child("host.child").scope(async {}).await }
        .with_context(host_context.clone())
        .await;
    host_context.span().end();

    // Assert
    let spans = exporter.get_finished_spans().expect("spans");
    let detached = spans
        .iter()
        .find(|span| span.name == "detached.unsampled")
        .expect("detached");
    assert_ne!(detached.span_context.trace_id(), remote.trace_id());
    assert_eq!(
        detached.links.links[0].span_context,
        *owned_context.span().span_context()
    );
    let child = spans
        .iter()
        .find(|span| span.name == "host.child")
        .expect("host child");
    assert_eq!(
        child.parent_span_id,
        host_context.span().span_context().span_id()
    );
    assert_eq!(
        child.span_context.trace_id(),
        host_context.span().span_context().trace_id()
    );
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
    let root_id = spans
        .iter()
        .find(|span| span.name == "session.turn")
        .map(|span| span.span_context.span_id());
    assert!(root_id.is_some());
    for name in ["utility.run", "reported.interval", "queue.wait"] {
        let child = spans.iter().find(|span| span.name == name);
        assert!(child.is_some());
        assert_eq!(child.map(|span| span.parent_span_id), root_id);
        if name == "reported.interval" {
            assert_eq!(
                child.map(|span| span.start_time),
                Some(SystemTime::UNIX_EPOCH)
            );
        }
    }
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
