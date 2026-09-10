//! Unit suites grouped by behavior.

#[path = "setting_test/support_test.rs"]
pub(crate) mod support;

#[path = "setting_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "setting_test/persistence_test.rs"]
mod persistence;

#[path = "setting_test/project_test.rs"]
mod project;

#[path = "setting_test/prompt_test.rs"]
mod prompt;

#[path = "setting_test/review_test.rs"]
mod review;

#[path = "setting_test/setting_test.rs"]
mod setting;

#[path = "setting_test/transition_test.rs"]
mod transition;
