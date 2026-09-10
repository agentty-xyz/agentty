//! Unit suites grouped by behavior.

#[path = "session_output_test/support_test.rs"]
pub(crate) mod support;

#[path = "session_output_test/display_test.rs"]
mod display;

#[path = "session_output_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "session_output_test/persistence_test.rs"]
mod persistence;

#[path = "session_output_test/prompt_test.rs"]
mod prompt;

#[path = "session_output_test/queue_test.rs"]
mod queue;

#[path = "session_output_test/review_test.rs"]
mod review;

#[path = "session_output_test/synchronization_test.rs"]
mod synchronization;

#[path = "session_output_test/transition_test.rs"]
mod transition;
