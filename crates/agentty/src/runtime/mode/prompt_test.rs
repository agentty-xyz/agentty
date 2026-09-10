//! Unit suites grouped by behavior.

#[path = "prompt_test/support_test.rs"]
pub(crate) mod support;

#[path = "prompt_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "prompt_test/persistence_test.rs"]
mod persistence;

#[path = "prompt_test/prompt_test.rs"]
mod prompt;

#[path = "prompt_test/queue_test.rs"]
mod queue;

#[path = "prompt_test/review_test.rs"]
mod review;

#[path = "prompt_test/synchronization_test.rs"]
mod synchronization;

#[path = "prompt_test/transition_test.rs"]
mod transition;
