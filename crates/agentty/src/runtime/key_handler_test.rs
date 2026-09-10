//! Unit suites grouped by behavior.

#[path = "key_handler_test/support_test.rs"]
pub(crate) mod support;

#[path = "key_handler_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "key_handler_test/project_test.rs"]
mod project;

#[path = "key_handler_test/question_test.rs"]
mod question;

#[path = "key_handler_test/queue_test.rs"]
mod queue;

#[path = "key_handler_test/review_test.rs"]
mod review;

#[path = "key_handler_test/state_test.rs"]
mod state;

#[path = "key_handler_test/synchronization_test.rs"]
mod synchronization;

#[path = "key_handler_test/transition_test.rs"]
mod transition;
