//! Unit suites grouped by behavior.

#[path = "state_test/support_test.rs"]
pub(crate) mod support;

#[path = "state_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "state_test/persistence_test.rs"]
mod persistence;

#[path = "state_test/project_test.rs"]
mod project;

#[path = "state_test/prompt_test.rs"]
mod prompt;

#[path = "state_test/question_test.rs"]
mod question;

#[path = "state_test/queue_test.rs"]
mod queue;

#[path = "state_test/review_test.rs"]
mod review;

#[path = "state_test/setting_test.rs"]
mod setting;

#[path = "state_test/state_test.rs"]
mod state;

#[path = "state_test/synchronization_test.rs"]
mod synchronization;

#[path = "state_test/transition_test.rs"]
mod transition;
