//! Maps how a native `bash` command exited onto its tool row, shared by
//! session rows and utility lifecycle rows.

use ag_contracts::{ActivityFacts, ActivityFailure, ActivityStatus};
use ag_harness::bash::{CommandExit, CommandTermination};

use crate::agent::activity_content::first_line;

/// Copies `exit` onto a completed command's row: its exit code, pipeline
/// statuses, and signal. A command that did not run to its end is judged
/// by its termination: one that hit its deadline or failed to run failed,
/// with how it exited as the reason, and a cancelled one was interrupted.
pub(super) fn apply_command_exit(
    status: &mut ActivityStatus,
    exit_code: &mut Option<i64>,
    facts: &mut ActivityFacts,
    exit: &CommandExit,
) {
    *exit_code = exit.exit_code.map(i64::from);
    facts.pipe_status = exit.pipe_status.iter().copied().map(i64::from).collect();
    facts.signal = exit.signal.map(i64::from);
    let (terminated, failure) = match exit.termination {
        CommandTermination::Deadline => (ActivityStatus::Failed, Some(ActivityFailure::TimedOut)),
        CommandTermination::Failed => (ActivityStatus::Failed, Some(ActivityFailure::Error)),
        CommandTermination::Cancelled => (ActivityStatus::Interrupted, None),
        _ => return,
    };
    *status = terminated;
    facts.failure = failure;
    facts.reason = failure.and_then(|_| first_line(&exit.to_string()));
}

#[cfg(test)]
#[path = "command_exit_test.rs"]
mod tests;
