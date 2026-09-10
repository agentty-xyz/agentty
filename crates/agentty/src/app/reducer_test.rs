use tokio::sync::mpsc;

use super::super::core::AppEvent;
use super::{APP_EVENT_DRAIN_BUDGET, AppEventReducer};

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
