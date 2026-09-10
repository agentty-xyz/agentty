//! Unit suites grouped by behavior.

#[path = "merge_test/support_test.rs"]
pub(crate) mod support;

#[path = "merge_test/state_test.rs"]
mod state;

#[path = "merge_test/synchronization_test.rs"]
mod synchronization;

#[path = "merge_test/transition_test.rs"]
mod transition;
