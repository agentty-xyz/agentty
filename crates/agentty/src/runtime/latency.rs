//! Foreground cycle latency sampling for slow-frame traces.

use std::time::{Duration, Instant};

use ag_telemetry::{KeyValue, Outcome, Span};

use crate::infra::clock::Clock;

/// Busy foreground time at or above which one cycle exports a
/// `ui.slow_cycle` trace. Faster cycles record nothing.
pub(crate) const SLOW_CYCLE_THRESHOLD: Duration = Duration::from_millis(50);

/// Busy time between one wake-up of the foreground loop and the frame drawn
/// after it, split by phase. The idle wait for input, runtime events, or the
/// frame tick is excluded, so the total is the input-to-frame latency.
#[derive(Default)]
pub(crate) struct CycleTiming {
    events: Duration,
    handled_events: usize,
    reduce: Duration,
    reload: Duration,
    render: Duration,
}

impl CycleTiming {
    /// Adds the time spent handling terminal input and runtime commands
    /// after the wait, and the number of terminal events handled.
    pub(crate) fn record_events(&mut self, handled_events: usize, elapsed: Duration) {
        self.handled_events = self.handled_events.saturating_add(handled_events);
        self.events = self.events.saturating_add(elapsed);
    }

    /// Adds the time spent reducing pending app events and reconciling
    /// state before drawing.
    pub(crate) fn record_reduce(&mut self, elapsed: Duration) {
        self.reduce = self.reduce.saturating_add(elapsed);
    }

    /// Adds the time spent refreshing sessions on a frame tick.
    pub(crate) fn record_reload(&mut self, elapsed: Duration) {
        self.reload = self.reload.saturating_add(elapsed);
    }

    /// Adds the time spent drawing the frame.
    pub(crate) fn record_render(&mut self, elapsed: Duration) {
        self.render = self.render.saturating_add(elapsed);
    }

    /// Returns the injected clock's elapsed time since `started_at`.
    pub(crate) fn elapsed_since(clock: &dyn Clock, started_at: Instant) -> Duration {
        clock.now_instant().saturating_duration_since(started_at)
    }

    /// Ends the measured cycle: when its busy time reaches
    /// [`SLOW_CYCLE_THRESHOLD`], exports one independent `ui.slow_cycle`
    /// trace starting when the cycle's work began. Resets the measurement
    /// either way.
    pub(crate) fn report(&mut self, clock: &dyn Clock) {
        let timing = std::mem::take(self);
        let busy = timing
            .events
            .saturating_add(timing.reduce)
            .saturating_add(timing.reload)
            .saturating_add(timing.render);
        if busy < SLOW_CYCLE_THRESHOLD {
            return;
        }

        let ended_at = clock.now_system_time();
        let started_at = ended_at.checked_sub(busy).unwrap_or(ended_at);
        Span::root_at("ui.slow_cycle", timing.attributes(), started_at).finish(Outcome::Completed);
    }

    /// Returns the content-free phase durations, event count, and the
    /// bounded name of the dominant phase.
    fn attributes(&self) -> Vec<KeyValue> {
        let phases = [
            ("events", self.events),
            ("reduce", self.reduce),
            ("reload", self.reload),
            ("render", self.render),
        ];
        let dominant_phase = phases
            .iter()
            .max_by_key(|(_, elapsed)| *elapsed)
            .map_or("events", |(phase, _)| *phase);

        vec![
            KeyValue::new(
                "agentty.ui.events",
                i64::try_from(self.handled_events).unwrap_or(i64::MAX),
            ),
            KeyValue::new("agentty.ui.events_ms", Self::millis(self.events)),
            KeyValue::new("agentty.ui.phase", dominant_phase),
            KeyValue::new("agentty.ui.reduce_ms", Self::millis(self.reduce)),
            KeyValue::new("agentty.ui.reload_ms", Self::millis(self.reload)),
            KeyValue::new("agentty.ui.render_ms", Self::millis(self.render)),
        ]
    }

    /// Converts one duration to whole milliseconds for an integer attribute.
    fn millis(elapsed: Duration) -> i64 {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
#[path = "latency_test.rs"]
mod tests;
