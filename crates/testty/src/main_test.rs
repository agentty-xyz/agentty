#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

use super::{Cli, Command, ProofCommand, run_scenario, run_scenario_reporting};

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
fn every_remaining_stub_reports_unimplemented_and_fails() {
    // Arrange — verbs whose behavior is not yet wired up.
    let verbs: [Command; 4] = [
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
    ];

    // Act + Assert
    for verb in verbs {
        assert_eq!(verb.dispatch(), ExitCode::FAILURE);
    }
}

#[test]
fn run_scenario_fails_when_file_missing() {
    // Arrange
    let missing = Path::new("/nonexistent/testty-scenario.yaml");

    // Act
    let code = run_scenario(missing, None, None);

    // Assert
    assert_eq!(code, ExitCode::FAILURE);
}

#[test]
fn run_scenario_fails_on_unsupported_version() {
    // Arrange — an otherwise valid scenario with an unknown version.
    let temp = tempfile::tempdir().expect("temp dir");
    let path = temp.path().join("bad.yaml");
    std::fs::write(&path, "version: 999\nsession:\n  bin: /bin/echo\n").expect("write");

    // Act
    let code = run_scenario(&path, None, None);

    // Assert
    assert_eq!(code, ExitCode::FAILURE);
}

/// The `Run` dispatch arm actually routes through `run_scenario`: a valid
/// scenario against a real fixture reports success, which a stub that
/// merely returned `FAILURE` could not produce.
#[cfg(unix)]
#[test]
fn dispatch_runs_scenario_for_run_verb() {
    // Arrange — a fixture that renders deterministic text and a scenario
    // that expects it, wired through the `Run` verb.
    let temp = tempfile::tempdir().expect("temp dir");
    let script = temp.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
             stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \"Hello \
             World\", region: [0, 0, 80, 1] }}\n",
            bin = script.display()
        ),
    )
    .expect("write scenario");
    let verb = Command::Run {
        scenario,
        bin: None,
        proof: None,
    };

    // Act
    let code = verb.dispatch();

    // Assert
    assert_eq!(code, ExitCode::SUCCESS);
}

#[test]
fn run_scenario_fails_when_binary_cannot_spawn() {
    // Arrange — a parseable scenario whose binary does not exist, so the
    // engine errors while spawning rather than producing expectations.
    let temp = tempfile::tempdir().expect("temp dir");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        "session:\n  bin: /nonexistent/testty-missing-binary\n  size: [80, 24]\nsteps:\n  - \
         wait_for_stable_frame: { stable_ms: 100, timeout_ms: 1000 }\n",
    )
    .expect("write scenario");

    // Act
    let code = run_scenario(&scenario, None, None);

    // Assert
    assert_eq!(code, ExitCode::FAILURE);
}

/// End-to-end: the `run` verb drives a real binary and reports success.
#[cfg(unix)]
#[test]
fn run_scenario_passes_against_fixture_binary() {
    // Arrange — a fixture that renders deterministic text and stays alive.
    let temp = tempfile::tempdir().expect("temp dir");
    let script = temp.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
             stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \"Hello \
             World\", region: [0, 0, 80, 1] }}\n",
            bin = script.display()
        ),
    )
    .expect("write scenario");

    // Act
    let code = run_scenario(&scenario, None, None);

    // Assert
    assert_eq!(code, ExitCode::SUCCESS);
}

/// `bin_override` (the `--bin` flag) replaces the scenario's `session.bin`:
/// the scenario points at a binary that cannot spawn, so success is only
/// possible when the override binary is the one actually driven.
#[cfg(unix)]
#[test]
fn run_scenario_honors_bin_override() {
    // Arrange — the real fixture is supplied through `bin_override`, while
    // the scenario points at a placeholder binary that must never spawn.
    let temp = tempfile::tempdir().expect("temp dir");
    let script = temp.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        "session:\n  bin: /nonexistent/placeholder\n  size: [80, 24]\nsteps:\n  - \
         wait_for_stable_frame: { stable_ms: 300, timeout_ms: 5000 }\nexpect:\n  - \
         text_in_region: { text: \"Hello World\", region: [0, 0, 80, 1] }\n",
    )
    .expect("write scenario");

    // Act
    let code = run_scenario(&scenario, Some(&script), None);

    // Assert
    assert_eq!(code, ExitCode::SUCCESS);
}

/// A `--proof` directory is not yet supported, so the run emits a notice on
/// the diagnostic sink and still drives the scenario to completion.
#[cfg(unix)]
#[test]
fn run_scenario_warns_when_proof_requested() {
    // Arrange — a passing fixture scenario plus a requested proof
    // directory.
    let temp = tempfile::tempdir().expect("temp dir");
    let script = temp.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
             stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \"Hello \
             World\", region: [0, 0, 80, 1] }}\n",
            bin = script.display()
        ),
    )
    .expect("write scenario");
    let proof = temp.path().join("proof-out");
    let mut out = Vec::new();

    // Act
    let code = run_scenario_reporting(&scenario, None, Some(&proof), &mut out);

    // Assert
    let log = String::from_utf8(out).expect("utf8 log");
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(log.contains("--proof is not yet supported"));
}

/// A scenario whose expectation does not match the rendered frame reports
/// the failed expectation on the diagnostic sink and exits with failure.
#[cfg(unix)]
#[test]
fn run_scenario_reports_failed_expectations() {
    // Arrange — the fixture renders "Hello World" but the scenario expects
    // different text.
    let temp = tempfile::tempdir().expect("temp dir");
    let script = temp.path().join("greet.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'Hello World'\nsleep 60\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).expect("perms");
    let scenario = temp.path().join("scenario.yaml");
    std::fs::write(
        &scenario,
        format!(
            "session:\n  bin: {bin}\n  size: [80, 24]\nsteps:\n  - wait_for_stable_frame: {{ \
             stable_ms: 300, timeout_ms: 5000 }}\nexpect:\n  - text_in_region: {{ text: \"Goodbye \
             Moon\", region: [0, 0, 80, 1] }}\n",
            bin = script.display()
        ),
    )
    .expect("write scenario");
    let mut out = Vec::new();

    // Act
    let code = run_scenario_reporting(&scenario, None, None, &mut out);

    // Assert
    let log = String::from_utf8(out).expect("utf8 log");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(log.contains("expectation(s) failed"));
}
