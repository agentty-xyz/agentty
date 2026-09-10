use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::{env, fs};

use crate::vhs::{VhsError, check_vhs_installed};

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
            let executable = directory.join("vhs");
            fs::write(
                &executable,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$TESTTY_VHS_PROBE_DIRECTORY/arguments\"\n",
            )
            .expect("write probe stub");
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o750))
                .expect("make probe executable");
        }

        // Act
        let output = Command::new(env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "vhs::tests::probe::installation_probe_handles_available_and_missing_vhs",
                "--nocapture",
            ])
            .env("PATH", &directory)
            .env("TESTTY_VHS_PROBE_CASE", case)
            .env("TESTTY_VHS_PROBE_DIRECTORY", &directory)
            .output()
            .expect("run isolated installation probe");

        // Assert
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}
