//! Unit tests for the Codex app-server lifecycle, split by behavior.

#[path = "lifecycle_test/payload_test.rs"]
mod payload;
#[path = "lifecycle_test/runtime_test.rs"]
mod runtime;
#[path = "lifecycle_test/steering_test.rs"]
mod steering;
#[path = "lifecycle_test/support_test.rs"]
mod support;
