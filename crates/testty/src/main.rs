//! Language-agnostic command-line front end for the `testty` framework.
//!
//! This binary ships inside the `testty` crate so non-Rust projects can drive
//! TUI end-to-end scenarios without writing Rust test harnesses. Installing the
//! crate (`cargo install testty`) provides the `testty` executable, which Cargo
//! auto-detects from this `src/main.rs` alongside the library `src/lib.rs`.
//!
//! The binary exposes the full `testty` verb tree (`run`, `schema`,
//! `proof open`, `proof gallery`, `update`).
//!
//! The command tree and argument parsing are complete, but every verb is
//! currently stubbed. Each stub reports "not yet implemented" on stderr and
//! exits with a non-zero status so callers and CI can detect that the behavior
//! is not wired up yet. Later tasks replace each [`Command::dispatch`] arm with
//! real scenario, schema, and proof logic that delegates into the `testty`
//! library API.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Top-level command-line interface for the `testty` runner.
#[derive(Parser)]
#[command(name = "testty", about, version)]
struct Cli {
    /// Selected verb to execute.
    #[command(subcommand)]
    command: Command,
}

/// Supported `testty` verbs.
///
/// Every variant is dispatched through [`Command::dispatch`], which currently
/// returns [`ExitCode::FAILURE`] for all verbs until the behavior is filled in.
#[derive(Subcommand)]
enum Command {
    /// Runs a scenario file against a TUI binary.
    Run {
        /// Path to the scenario definition (for example, `scenario.yaml`).
        scenario: PathBuf,
        /// Path to the binary under test, overriding the scenario default.
        #[arg(long)]
        bin: Option<PathBuf>,
        /// Output directory for generated proof artifacts.
        #[arg(long)]
        proof: Option<PathBuf>,
    },
    /// Prints the JSON schema for scenario files.
    Schema,
    /// Inspects proof artifacts produced by a run.
    Proof {
        /// Selected proof subcommand.
        #[command(subcommand)]
        command: ProofCommand,
    },
    /// Updates stored scenario snapshots to match current output.
    Update,
}

impl Command {
    /// Executes the selected verb and returns the process exit code.
    ///
    /// All arms are stubbed: each writes a "not yet implemented" notice to
    /// stderr and returns [`ExitCode::FAILURE`]. Replace the individual arms as
    /// the verbs are implemented.
    fn dispatch(self) -> ExitCode {
        match self {
            Command::Run { .. } => not_implemented("run"),
            Command::Schema => not_implemented("schema"),
            Command::Proof {
                command: ProofCommand::Open { .. },
            } => not_implemented("proof open"),
            Command::Proof {
                command: ProofCommand::Gallery { .. },
            } => not_implemented("proof gallery"),
            Command::Update => not_implemented("update"),
        }
    }
}

/// Subcommands for inspecting proof artifacts.
#[derive(Subcommand)]
enum ProofCommand {
    /// Opens a single proof report in the browser.
    Open {
        /// Path to the proof HTML report.
        html: PathBuf,
    },
    /// Builds a gallery from a directory of proof reports.
    Gallery {
        /// Directory containing proof reports.
        dir: PathBuf,
    },
}

/// Parses arguments and dispatches the selected verb.
fn main() -> ExitCode {
    let cli = Cli::parse();

    cli.command.dispatch()
}

/// Reports an unimplemented verb on stderr and returns a failing exit code.
///
/// Writing directly to stderr (instead of `eprintln!`) keeps the stub free of
/// the `print_stderr` lint while still surfacing the notice to callers.
fn not_implemented(verb: &str) -> ExitCode {
    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "testty: `{verb}` is not yet implemented");

    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn parses_run_with_scenario_path() {
        // Arrange
        let argv = ["testty", "run", "scenario.yaml"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Run { scenario, bin: None, proof: None }
                if scenario.as_path() == Path::new("scenario.yaml")
        ));
    }

    #[test]
    fn parses_run_with_bin_and_proof_options() {
        // Arrange
        let argv = [
            "testty",
            "run",
            "scenario.yaml",
            "--bin",
            "./app",
            "--proof",
            "out",
        ];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Run { bin: Some(bin), proof: Some(proof), .. }
                if bin.as_path() == Path::new("./app") && proof.as_path() == Path::new("out")
        ));
    }

    #[test]
    fn parses_schema_verb() {
        // Arrange
        let argv = ["testty", "schema"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(cli.command, Command::Schema));
    }

    #[test]
    fn parses_proof_open_with_html_path() {
        // Arrange
        let argv = ["testty", "proof", "open", "report.html"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Proof { command: ProofCommand::Open { html } }
                if html.as_path() == Path::new("report.html")
        ));
    }

    #[test]
    fn parses_proof_gallery_with_dir_path() {
        // Arrange
        let argv = ["testty", "proof", "gallery", "proofs"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(
            cli.command,
            Command::Proof { command: ProofCommand::Gallery { dir } }
                if dir.as_path() == Path::new("proofs")
        ));
    }

    #[test]
    fn parses_update_verb() {
        // Arrange
        let argv = ["testty", "update"];

        // Act
        let cli = Cli::parse_from(argv);

        // Assert
        assert!(matches!(cli.command, Command::Update));
    }

    #[test]
    fn run_without_scenario_is_rejected() {
        // Arrange
        let argv = ["testty", "run"];

        // Act
        let result = Cli::try_parse_from(argv);

        // Assert
        assert!(result.is_err());
    }

    #[test]
    fn every_stub_reports_unimplemented_and_fails() {
        // Arrange
        let verbs: [Command; 6] = [
            Command::Run {
                scenario: PathBuf::from("s.yaml"),
                bin: None,
                proof: None,
            },
            Command::Schema,
            Command::Proof {
                command: ProofCommand::Open {
                    html: PathBuf::from("r.html"),
                },
            },
            Command::Proof {
                command: ProofCommand::Gallery {
                    dir: PathBuf::from("d"),
                },
            },
            Command::Update,
            Command::Run {
                scenario: PathBuf::from("s.yaml"),
                bin: Some(PathBuf::from("b")),
                proof: Some(PathBuf::from("p")),
            },
        ];

        // Act + Assert
        for verb in verbs {
            assert_eq!(verb.dispatch(), ExitCode::FAILURE);
        }
    }
}
