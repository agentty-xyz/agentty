//! Unit suites grouped by behavior.

#[path = "task_test/support_test.rs"]
pub(crate) mod support;

#[path = "task_test/display_test.rs"]
mod display;

#[path = "task_test/lifecycle_test.rs"]
mod lifecycle;

#[path = "task_test/persistence_test.rs"]
mod persistence;

#[path = "task_test/project_test.rs"]
mod project;

#[path = "task_test/prompt_test.rs"]
mod prompt;

#[path = "task_test/review_test.rs"]
mod review;

#[path = "task_test/setting_test.rs"]
mod setting;

#[path = "task_test/state_test.rs"]
mod state;

#[path = "task_test/transition_test.rs"]
mod transition;
