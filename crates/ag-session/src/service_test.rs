//! Unit tests for the session service, split by behavior.

#[path = "service_test/delegation_test.rs"]
mod delegation;

#[path = "service_test/error_test.rs"]
mod error;

#[path = "service_test/support_test.rs"]
mod support;

#[path = "service_test/task_test.rs"]
mod task;
