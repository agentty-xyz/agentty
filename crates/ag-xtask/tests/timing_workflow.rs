//! The source workflow must retain each hook's outcome and keep running peers.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use serde_yaml_ng::Value;

#[test]
fn source_loop_records_each_outcome_without_stale_results() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workflow: Value =
        serde_yaml_ng::from_str(include_str!("../../../.github/workflows/postsubmit.yml"))?;
    let step = workflow["jobs"]["source-tests"]["steps"]
        .as_sequence()
        .ok_or("steps")?
        .iter()
        .find(|step| step["id"].as_str() == Some("source-tests"))
        .ok_or("source step")?;
    let script = step["run"].as_str().ok_or("run command")?;
    let directory = tempfile::tempdir()?;
    for (name, contents) in [
        ("prek", "#!/bin/sh\n[ \"$2\" != bad ]\n"),
        (
            "node",
            "#!/bin/sh\nprintf '%s %s %s\\n' \"$INPUT_SUITE\" \"$INPUT_PHASE\" \
             \"${INPUT_OUTCOME:-}\" >> calls\n",
        ),
    ] {
        let path = directory.path().join(name);
        fs::write(&path, contents)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    fs::create_dir_all(directory.path().join("target/test-results/old"))?;
    fs::write(
        directory
            .path()
            .join("target/test-results/old/metadata.json"),
        "stale",
    )?;
    // Act
    let result = Command::new("bash")
        .args(["-e", "-c", script])
        .current_dir(directory.path())
        .env_clear()
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", directory.path().display()),
        )
        .env("SOURCE_TEST_HOOKS", "good bad last")
        .output()?;
    // Assert
    assert!(!result.status.success());
    assert!(!directory.path().join("target/test-results/old").exists());
    let calls = fs::read_to_string(directory.path().join("calls"))?;
    assert_eq!(
        calls,
        "good prepare \ngood finish success\nbad prepare success\nbad finish failure\nlast \
         prepare failure\nlast finish success\n"
    );

    Ok(())
}
