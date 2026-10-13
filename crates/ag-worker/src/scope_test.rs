use ag_telemetry::{Context, TraceContextExt as _};
use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};

use super::{CURRENT, RunContext};

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

fn context(trace: u128, span: u64) -> Context {
    Context::new().with_remote_span_context(SpanContext::new(
        TraceId::from(trace),
        SpanId::from(span),
        TraceFlags::default(),
        true,
        TraceState::default(),
    ))
}
