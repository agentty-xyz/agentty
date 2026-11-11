use std::sync::Arc;

use ag_telemetry::{Context, TraceContextExt as _};
use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

use super::{CURRENT, RunContext, with_execution_resource};

#[tokio::test]
async fn inherited_phase_links_must_match_the_captured_ownership_trace() {
    // Arrange / Act / Assert
    for trace_id in [1, 2] {
        let parent = RunContext {
            trace: context(2, 1),
            phase_trace: Some(context(2, 2)),
            ..RunContext::default()
        };
        let captured = RunContext {
            trace: context(trace_id, 3),
            ..RunContext::default()
        };
        let inherited = CURRENT.scope(parent, async { captured.inherited() }).await;
        assert_eq!(
            inherited.trace.span().span_context().trace_id(),
            TraceId::from(trace_id)
        );
        assert_eq!(inherited.phase_trace.is_some(), trace_id == 2);
    }
}

#[tokio::test]
async fn execution_resources_are_retained_only_while_scoped_work_runs() {
    // Arrange
    let resource = Arc::new(());

    // Act
    let (retained_count, resource_count) = with_execution_resource(Arc::clone(&resource), async {
        (
            RunContext::current().resources.len(),
            Arc::strong_count(&resource),
        )
    })
    .await;

    // Assert
    assert_eq!(retained_count, 1);
    assert_eq!(resource_count, 2);
    assert_eq!(Arc::strong_count(&resource), 1);
}

fn context(trace: u128, span: u64) -> Context {
    Context::new().with_remote_span_context(SpanContext::new(
        TraceId::from(trace),
        SpanId::from(span),
        TraceFlags::default(),
        true,
        TraceState::default(),
    ))
}
