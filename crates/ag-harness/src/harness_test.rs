//! Unit tests for `Harness` turns and sessions, split by behavior. Suites
//! that inspect SQLite persistence need the `sqlite` feature.

#[path = "harness_test/builder_test.rs"]
mod builder;
#[path = "harness_test/calibration_test.rs"]
mod calibration;
#[cfg(feature = "sqlite")]
#[path = "harness_test/cancellation_test.rs"]
mod cancellation;
#[path = "harness_test/fault_test.rs"]
mod fault;
#[expect(
    deprecated,
    reason = "legacy-mapping coverage: the deprecated `ToolPolicy` must send the same definitions \
              and record the same host requests as the equivalent set"
)]
#[path = "harness_test/handler_test.rs"]
mod handler;
#[path = "harness_test/history_test.rs"]
mod history;
#[expect(
    deprecated,
    reason = "legacy-mapping coverage: options configured through the deprecated `ToolPolicy` \
              keep their stored snapshots and replay"
)]
#[cfg(feature = "sqlite")]
#[path = "harness_test/options_test.rs"]
mod options;
#[path = "harness_test/policy_test.rs"]
mod policy;
#[path = "harness_test/read_test.rs"]
mod read;
#[cfg(feature = "sqlite")]
#[path = "harness_test/session_test.rs"]
mod session;
#[path = "harness_test/summary_test.rs"]
mod summary;
#[path = "harness_test/support_test.rs"]
mod support;
#[path = "harness_test/tool_loop_test.rs"]
mod tool_loop;
#[cfg(feature = "sqlite")]
#[path = "harness_test/write_journal_test.rs"]
mod write_journal;

#[expect(
    deprecated,
    reason = "legacy-mapping coverage: fingerprints recorded through the deprecated `ToolPolicy` \
              stay recognizable"
)]
#[path = "harness_test/recovery_test.rs"]
mod recovery;
