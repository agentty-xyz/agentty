//! Process-level contracts for the optional compiler cache wrapper.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn cache_failures_fall_back_without_masking_compiler_results() -> Result<(), Box<dyn Error>> {
    for (cache_status, compiler_status, expected_status, compiler_runs) in [
        (0, 42, 0, false),
        (1, 0, 1, false),
        (2, 0, 2, false),
        (101, 0, 101, false),
        (130, 0, 130, false),
        (254, 0, 0, true),
        (254, 42, 42, true),
    ] {
        // Arrange
        let directory = tempfile::tempdir()?;
        let wrapper = directory.path().join("wrapper");
        fs::write(&wrapper, include_str!("../../../.cargo/rustc-wrapper.sh"))?;
        let cache = directory.path().join("sccache");
        fs::write(
            &cache,
            "#!/bin/sh\nif [ \"$1\" = --dist-status ]; then exit 0; fi\nprintf '%s\\0' \"$@\" > \
             cache-arguments\nexit \"$CACHE_STATUS\"\n",
        )?;
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755))?;
        let compiler = directory.path().join("compiler with spaces");
        fs::write(
            &compiler,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > compiler-arguments\nexit \"$COMPILER_STATUS\"\n",
        )?;
        fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755))?;
        let arguments = [
            "--cfg",
            "feature=\"with spaces\"",
            "",
            "$(touch unexpected)",
        ];

        // Act
        let output = Command::new("bash")
            .arg(&wrapper)
            .arg(&compiler)
            .args(arguments)
            .current_dir(directory.path())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", directory.path().display()),
            )
            .env("CACHE_STATUS", cache_status.to_string())
            .env("COMPILER_STATUS", compiler_status.to_string())
            .output()?;

        // Assert
        assert_eq!(output.status.code(), Some(expected_status));
        let expected_arguments = format!("{}\0", arguments.join("\0"));
        assert_eq!(
            fs::read_to_string(directory.path().join("cache-arguments"))?,
            format!("{}\0{expected_arguments}", compiler.display())
        );
        let compiler_arguments = directory.path().join("compiler-arguments");
        assert_eq!(compiler_arguments.exists(), compiler_runs);
        if compiler_runs {
            assert_eq!(fs::read_to_string(compiler_arguments)?, expected_arguments);
            assert!(String::from_utf8_lossy(&output.stderr).contains("retrying"));
        }
        assert!(!directory.path().join("unexpected").exists());
    }

    Ok(())
}
