//! Trusted native launcher for the matching ag-harness library version.

fn main() -> std::process::ExitCode {
    ag_harness::run_sandbox_launcher()
}
