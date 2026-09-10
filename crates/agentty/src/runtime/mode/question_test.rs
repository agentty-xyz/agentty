//! Unit suites grouped by behavior.

#[path = "question_test/support_test.rs"]
pub(crate) mod support;

#[path = "question_test/display_test.rs"]
mod display;

#[path = "question_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "question_test/prompt_test.rs"]
mod prompt;

#[path = "question_test/question_test.rs"]
mod question;

#[path = "question_test/queue_test.rs"]
mod queue;

#[path = "question_test/review_test.rs"]
mod review;

#[path = "question_test/state_test.rs"]
mod state;

#[path = "question_test/transition_test.rs"]
mod transition;
