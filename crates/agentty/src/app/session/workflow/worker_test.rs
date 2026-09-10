//! Unit suites grouped by behavior.

#[path = "worker_test/support_test.rs"]
pub(crate) mod support;

#[path = "worker_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "worker_test/persistence_test.rs"]
mod persistence;

#[path = "worker_test/prompt_test.rs"]
mod prompt;

#[path = "worker_test/question_test.rs"]
mod question;

#[path = "worker_test/queue_test.rs"]
mod queue;

#[path = "worker_test/review_test.rs"]
mod review;

#[path = "worker_test/state_test.rs"]
mod state;

#[path = "worker_test/synchronization_test.rs"]
mod synchronization;

#[path = "worker_test/transition_test.rs"]
mod transition;
