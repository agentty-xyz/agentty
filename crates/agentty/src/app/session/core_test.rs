//! Unit suites grouped by behavior.

#[path = "core_test/support_test.rs"]
pub(crate) mod support;

#[path = "core_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "core_test/persistence_test.rs"]
mod persistence;

#[path = "core_test/project_test.rs"]
mod project;

#[path = "core_test/prompt_test.rs"]
mod prompt;

#[path = "core_test/question_test.rs"]
mod question;

#[path = "core_test/queue_test.rs"]
mod queue;

#[path = "core_test/review_test.rs"]
mod review;

#[path = "core_test/state_test.rs"]
mod state;

#[path = "core_test/synchronization_test.rs"]
mod synchronization;

#[path = "core_test/transition_test.rs"]
mod transition;
