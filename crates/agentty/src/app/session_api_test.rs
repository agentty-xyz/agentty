//! Unit suites grouped by behavior.

#[path = "session_api_test/support_test.rs"]
pub(crate) mod support;

#[path = "session_api_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "session_api_test/persistence_test.rs"]
mod persistence;

#[path = "session_api_test/project_test.rs"]
mod project;

#[path = "session_api_test/question_test.rs"]
mod question;

#[path = "session_api_test/queue_test.rs"]
mod queue;

#[path = "session_api_test/review_test.rs"]
mod review;

#[path = "session_api_test/setting_test.rs"]
mod setting;

#[path = "session_api_test/state_test.rs"]
mod state;

#[path = "session_api_test/synchronization_test.rs"]
mod synchronization;

#[path = "session_api_test/transition_test.rs"]
mod transition;
