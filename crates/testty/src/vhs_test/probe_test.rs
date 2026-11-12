use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use crate::scenario::Scenario;
use crate::vhs::{VhsError, VhsTape, check_vhs_installed};

/// Stub `vhs` preamble that answers the installation probe successfully.
const VERSION_PROBE_PREAMBLE: &str = "#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\n";

#[test]
fn installation_probe_handles_available_and_missing_vhs() {
    // Arrange — subprocesses isolate PATH without changing the test runner's
    // environment or invoking the host's VHS installation.
    if let Ok(case) = env::var("TESTTY_VHS_PROBE_CASE") {
        // Act
        let result = check_vhs_installed();

        // Assert
        if case == "available" {
            assert!(result.is_ok());
            let directory = env::var("TESTTY_VHS_PROBE_DIRECTORY").expect("probe directory");
            let arguments = fs::read_to_string(Path::new(&directory).join("arguments"))
                .expect("stub should record its arguments");
            assert_eq!(arguments, "--version\n");
        } else {
            assert!(matches!(result, Err(VhsError::NotInstalled(message))
                if message.contains("Install with: brew install vhs")));
        }

        return;
    }

    let temp_dir = tempfile::tempdir().expect("probe fixture directory");
    for case in ["available", "missing"] {
        let directory = temp_dir.path().join(case);
        fs::create_dir(&directory).expect("create isolated PATH");
        if case == "available" {
            write_vhs_stub(
                &directory,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TESTTY_VHS_PROBE_DIRECTORY/arguments\"\n",
            );
        }

        // Act / Assert
        run_isolated_case(
            "vhs::tests::probe::installation_probe_handles_available_and_missing_vhs",
            case,
            &directory,
        );
    }
}

/// Verifies `VhsTape::execute` against stub `vhs` executables for each
/// outcome: an unwritable tape, a recorder that cannot be launched, a failed
/// recording, a recording without a screenshot, and a successful recording.
#[test]
fn execute_reports_each_recorder_outcome() {
    // Arrange — each case runs in a subprocess whose PATH holds only its stub.
    if let Ok(case) = env::var("TESTTY_VHS_PROBE_CASE") {
        let directory = PathBuf::from(env::var("TESTTY_VHS_PROBE_DIRECTORY").expect("directory"));
        let screenshot_path = directory.join("shot.png");
        let tape = VhsTape::from_scenario(
            &Scenario::new("demo").capture(),
            Path::new("/bin/true"),
            &screenshot_path,
            &[],
        );
        let tape_path = if case == "unwritable" {
            directory.join("missing").join("demo.tape")
        } else {
            directory.join("demo.tape")
        };

        // Act
        let result = tape.execute(&tape_path);

        // Assert
        match case.as_str() {
            "unwritable" => assert!(matches!(result, Err(VhsError::IoError(_)))),
            "vanishing" => assert!(matches!(result, Err(VhsError::ExecutionFailed(_)))),
            "failing" => assert!(matches!(result, Err(VhsError::ExecutionFailed(message))
                if message == "VHS exited with error: tape broke")),
            "silent" => {
                assert!(
                    matches!(result, Err(VhsError::ScreenshotNotProduced(message))
                    if message.starts_with("Attempt 3/3: screenshot not produced at"))
                );
                let runs = fs::read_to_string(directory.join("runs")).expect("stub runs");
                assert_eq!(runs, "run\nrun\nrun\n");
            }
            _ => {
                assert_eq!(result.ok(), Some(screenshot_path));
                let written = fs::read_to_string(&tape_path).expect("tape should be written");
                assert_eq!(written, tape.render());
                let arguments = fs::read_to_string(directory.join("arguments")).expect("args");
                assert_eq!(arguments, tape_path.display().to_string());
            }
        }

        return;
    }

    let cases = [
        ("unwritable", format!("{VERSION_PROBE_PREAMBLE}exit 0\n")),
        (
            "vanishing",
            "#!/bin/sh\n/bin/rm -f \"$0\"\nexit 0\n".to_string(),
        ),
        (
            "failing",
            format!("{VERSION_PROBE_PREAMBLE}printf 'tape broke' >&2\nexit 3\n"),
        ),
        (
            "silent",
            format!(
                "{VERSION_PROBE_PREAMBLE}printf 'run\\n' >> \"$TESTTY_VHS_PROBE_DIRECTORY/runs\"\n"
            ),
        ),
        (
            "recorded",
            format!(
                "{VERSION_PROBE_PREAMBLE}printf '%s' \"$1\" > \
                 \"$TESTTY_VHS_PROBE_DIRECTORY/arguments\"\n: > \
                 \"$TESTTY_VHS_PROBE_DIRECTORY/shot.png\"\n"
            ),
        ),
    ];
    let temp_dir = tempfile::tempdir().expect("execute fixture directory");
    for (case, stub) in cases {
        let directory = temp_dir.path().join(case);
        fs::create_dir(&directory).expect("create isolated PATH");
        write_vhs_stub(&directory, &stub);

        // Act / Assert
        run_isolated_case(
            "vhs::tests::probe::execute_reports_each_recorder_outcome",
            case,
            &directory,
        );
    }
}

/// Writes an executable `vhs` stub script into `directory`.
fn write_vhs_stub(directory: &Path, script: &str) {
    let executable = directory.join("vhs");
    fs::write(&executable, script).expect("write vhs stub");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o750))
        .expect("make vhs stub executable");
}

/// Re-runs `test_name` in a subprocess whose `PATH` is only `directory`, and
/// asserts that the isolated `case` passed.
fn run_isolated_case(test_name: &str, case: &str, directory: &Path) {
    let output = Command::new(env::current_exe().expect("test executable"))
        .args(["--exact", test_name, "--nocapture"])
        .env("PATH", directory)
        .env("TESTTY_VHS_PROBE_CASE", case)
        .env("TESTTY_VHS_PROBE_DIRECTORY", directory)
        .output()
        .expect("run isolated vhs case");

    assert!(
        output.status.success(),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}
