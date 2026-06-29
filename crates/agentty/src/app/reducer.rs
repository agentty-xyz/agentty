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
mod tests {
    use tokio::sync::mpsc;

    use super::*;

    #[test]
    fn drain_keeps_events_over_budget_queued_for_later_cycle() {
        // Arrange
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        for _ in 0..(APP_EVENT_DRAIN_BUDGET + 2) {
            event_tx
                .send(AppEvent::RefreshSessions)
                .expect("event receiver should be open");
        }

        // Act
        let drained_events = AppEventReducer::drain(&mut event_rx, AppEvent::RefreshProjects);

        // Assert
        assert_eq!(drained_events.len(), APP_EVENT_DRAIN_BUDGET);
        assert_eq!(event_rx.len(), 3);
        assert!(matches!(
            drained_events.first(),
            Some(AppEvent::RefreshProjects)
        ));
    }
}
