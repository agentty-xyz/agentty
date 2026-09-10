//! App-event batch reduction helpers.

use tokio::sync::mpsc;
use tracing::debug;

use super::core::AppEvent;

/// Maximum app events reduced in one foreground cycle.
///
/// The first event that woke the reducer counts toward this budget. Any
/// remaining queued events stay in the channel for a later cycle so redraws and
/// ticks cannot be starved by a chatty producer.
pub(crate) const APP_EVENT_DRAIN_BUDGET: usize = 128;

/// Reducer utilities for draining and coalescing queued app events.
pub(crate) struct AppEventReducer;

impl AppEventReducer {
    /// Drains up to [`APP_EVENT_DRAIN_BUDGET`] app events into one ordered
    /// vector.
    pub(crate) fn drain(
        event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
        first_event: AppEvent,
    ) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        for _ in 1..APP_EVENT_DRAIN_BUDGET {
            let Ok(event) = event_rx.try_recv() else {
                break;
            };

            events.push(event);
        }

        let remaining_events = event_rx.len();
        if remaining_events > 0 {
            debug!(
                budget = APP_EVENT_DRAIN_BUDGET,
                drained_events = events.len(),
                remaining_events,
                "app event drain budget exhausted with queued events remaining"
            );
        }

        events
    }
}

#[cfg(test)]
#[path = "reducer_test.rs"]
mod tests;
