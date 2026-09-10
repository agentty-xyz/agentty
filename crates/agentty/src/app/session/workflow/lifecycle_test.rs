//! Unit suites grouped by behavior.

#[path = "lifecycle_test/support_test.rs"]
pub(crate) mod support;

#[path = "lifecycle_test/display_test.rs"]
mod display;

#[path = "lifecycle_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "lifecycle_test/persistence_test.rs"]
mod persistence;

#[path = "lifecycle_test/prompt_test.rs"]
mod prompt;

#[path = "lifecycle_test/question_test.rs"]
mod question;

#[path = "lifecycle_test/queue_test.rs"]
mod queue;

#[path = "lifecycle_test/review_test.rs"]
mod review;

#[path = "lifecycle_test/setting_test.rs"]
mod setting;

#[path = "lifecycle_test/state_test.rs"]
mod state;

#[path = "lifecycle_test/synchronization_test.rs"]
mod synchronization;

#[path = "lifecycle_test/transition_test.rs"]
mod transition;
