//! Unit tests for VHS tape compilation and execution, split by behavior.

#[path = "vhs_test/tape_test.rs"]
mod tape;

#[path = "vhs_test/step_test.rs"]
mod step;

#[cfg(unix)]
#[path = "vhs_test/probe_test.rs"]
mod probe;
