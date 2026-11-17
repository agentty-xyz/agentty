//! Slow foreground cycle trace tests.

use std::time::{Duration, Instant, SystemTime};

use ag_telemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};

use super::{CycleTiming, SLOW_CYCLE_THRESHOLD};
use crate::test_support::FixedClock;
use crate::test_support::telemetry::TRACER_PROVIDER_LOCK;

/// Wall-clock time the fixed test clock reports when a cycle ends.
fn cycle_end() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
}

/// Returns the finished `ui.slow_cycle` spans.
fn slow_cycles(exporter: &InMemorySpanExporter) -> Vec<SpanData> {
    exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|span| span.name == "ui.slow_cycle")
        .collect()
}

/// Installs an in-memory exporter as the process-global trace provider.
fn install_exporter() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );

    exporter
}

#[tokio::test]
async fn cycles_below_the_threshold_record_nothing() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_exporter();
    let clock = FixedClock::new(Instant::now(), cycle_end());
    let mut timing = CycleTiming::default();
    let just_below = SLOW_CYCLE_THRESHOLD
        .checked_sub(Duration::from_millis(1))
        .expect("positive threshold");
    timing.record_events(3, just_below / 2);
    timing.record_render(just_below / 2);

    // Act
    timing.report(&clock);

    // Assert
    assert_eq!(slow_cycles(&exporter), Vec::<SpanData>::new());
}

#[tokio::test]
async fn slow_cycle_exports_phase_durations_from_when_its_work_began() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_exporter();
    let clock = FixedClock::new(Instant::now(), cycle_end());
    let mut timing = CycleTiming::default();
    timing.record_events(2, Duration::from_millis(5));
    timing.record_events(1, Duration::from_millis(5));
    timing.record_reduce(Duration::from_millis(12));
    timing.record_reload(Duration::from_millis(40));
    timing.record_render(Duration::from_millis(8));

    // Act
    timing.report(&clock);
    timing.report(&clock);

    // Assert
    let spans = slow_cycles(&exporter);
    assert_eq!(spans.len(), 1, "the report resets the measurement");
    let span = &spans[0];
    assert_eq!(span.parent_span_id, SpanId::INVALID);
    assert_eq!(span.start_time, cycle_end() - Duration::from_millis(70));
    let mut attributes = span.attributes.clone();
    attributes.sort_by(|left, right| left.key.as_str().cmp(right.key.as_str()));
    assert_eq!(
        attributes,
        vec![
            KeyValue::new("agentty.outcome", "completed"),
            KeyValue::new("agentty.ui.events", 3_i64),
            KeyValue::new("agentty.ui.events_ms", 10_i64),
            KeyValue::new("agentty.ui.phase", "reload"),
            KeyValue::new("agentty.ui.reduce_ms", 12_i64),
            KeyValue::new("agentty.ui.reload_ms", 40_i64),
            KeyValue::new("agentty.ui.render_ms", 8_i64),
        ]
    );
}

#[tokio::test]
async fn cycle_at_the_threshold_is_traced_with_its_dominant_phase() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_exporter();
    let clock = FixedClock::new(Instant::now(), cycle_end());
    let mut timing = CycleTiming::default();
    timing.record_render(SLOW_CYCLE_THRESHOLD);

    // Act
    timing.report(&clock);

    // Assert
    let spans = slow_cycles(&exporter);
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].start_time, cycle_end() - SLOW_CYCLE_THRESHOLD);
    assert!(
        spans[0]
            .attributes
            .contains(&KeyValue::new("agentty.ui.phase", "render"))
    );
}
