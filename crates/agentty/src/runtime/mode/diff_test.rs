//! Unit suites grouped by behavior.

#[path = "diff_test/support_test.rs"]
pub(crate) mod support;

#[path = "diff_test/display_test.rs"]
mod display;

#[path = "diff_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "diff_test/persistence_test.rs"]
mod persistence;

#[path = "diff_test/prompt_test.rs"]
mod prompt;

#[path = "diff_test/question_test.rs"]
mod question;

#[path = "diff_test/review_test.rs"]
mod review;

#[path = "diff_test/synchronization_test.rs"]
mod synchronization;

#[path = "diff_test/transition_test.rs"]
mod transition;
