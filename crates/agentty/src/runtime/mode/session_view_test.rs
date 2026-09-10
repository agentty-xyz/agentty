//! Unit suites grouped by behavior.

#[path = "session_view_test/support_test.rs"]
pub(crate) mod support;

#[path = "session_view_test/display_test.rs"]
mod display;

#[path = "session_view_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "session_view_test/persistence_test.rs"]
mod persistence;

#[path = "session_view_test/project_test.rs"]
mod project;

#[path = "session_view_test/prompt_test.rs"]
mod prompt;

#[path = "session_view_test/queue_test.rs"]
mod queue;

#[path = "session_view_test/review_test.rs"]
mod review;

#[path = "session_view_test/state_test.rs"]
mod state;

#[path = "session_view_test/synchronization_test.rs"]
mod synchronization;

#[path = "session_view_test/transition_test.rs"]
mod transition;
