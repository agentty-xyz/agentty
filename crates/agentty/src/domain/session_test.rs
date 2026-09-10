//! Unit suites grouped by behavior.

#[path = "session_test/support_test.rs"]
pub(crate) mod support;

#[path = "session_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "session_test/prompt_test.rs"]
mod prompt;

#[path = "session_test/question_test.rs"]
mod question;

#[path = "session_test/queue_test.rs"]
mod queue;

#[path = "session_test/review_test.rs"]
mod review;

#[path = "session_test/setting_test.rs"]
mod setting;

#[path = "session_test/state_test.rs"]
mod state;

#[path = "session_test/synchronization_test.rs"]
mod synchronization;

#[path = "session_test/transition_test.rs"]
mod transition;
