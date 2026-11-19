//! Trusted native launcher for the matching ag-harness library version.

use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    ag_harness::sandbox::dispatch().unwrap_or_else(manual_start)
}

/// The harness always starts the helper with its helper argument, so only a
/// manual start reaches this.
fn manual_start() -> ExitCode {
    let _ = writeln!(
        io::stderr(),
        "ag-harness-sandbox is started by ag-harness; it has no command line of its own"
    );

    ExitCode::FAILURE
}
