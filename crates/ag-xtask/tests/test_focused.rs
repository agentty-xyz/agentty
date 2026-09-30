//! Process-level contracts for the cataloged focused-test hook.

#![cfg(unix)]

use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{fs, io};

use serde_yaml_ng::Value;
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    entry: String,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let cargo = directory.path().join("cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$AGENTTY_TEST_ARGUMENTS\"\nexit \
             \"${AGENTTY_TEST_EXIT_CODE:-0}\"\n",
        )?;
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))?;
        let catalog: Value =
            serde_yaml_ng::from_str(include_str!("../../../.pre-commit-config.yaml"))?;
        let hook = catalog["repos"]
            .as_sequence()
            .ok_or_else(|| io::Error::other("hook catalog has no repository list"))?
            .iter()
            .map(|repo| {
                repo["hooks"]
                    .as_sequence()
                    .ok_or_else(|| io::Error::other("hook repository has no hook list"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .find(|hook| hook["id"].as_str() == Some("test-focused"))
            .ok_or_else(|| io::Error::other("hook catalog has no focused hook"))?;
        let entry = hook["entry"]
            .as_str()
            .ok_or_else(|| io::Error::other("focused hook has no command"))?
            .to_string();

        Ok(Self { directory, entry })
    }

    fn run(
        &self,
        filter: Option<&str>,
        packages: Option<&str>,
        exit_code: i32,
    ) -> io::Result<Output> {
        let mut command = Command::new("sh");
        command
            .args(["-c", &self.entry])
            .current_dir(self.directory.path())
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.directory.path().display()),
            )
            .env("AGENTTY_TEST_ARGUMENTS", self.arguments_path())
            .env("AGENTTY_TEST_EXIT_CODE", exit_code.to_string())
            .env_remove("AGENTTY_TEST_FILTER")
            .env_remove("AGENTTY_TEST_PACKAGES");
        if let Some(filter) = filter {
            command.env("AGENTTY_TEST_FILTER", filter);
        }
        if let Some(packages) = packages {
            command.env("AGENTTY_TEST_PACKAGES", packages);
        }

        command.output()
    }

    fn arguments(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let bytes = fs::read(self.arguments_path())?;

        Ok(bytes
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .map(|argument| String::from_utf8(argument.to_vec()))
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn arguments_path(&self) -> PathBuf {
        self.directory.path().join("arguments")
    }
}

#[test]
fn default_build_scope_preserves_public_tests_and_separate_e2e_gate() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let output = fixture.run(Some("package(=ag-git)"), None, 0)?;
    let arguments = fixture.arguments()?;

    // Assert
    assert!(output.status.success());
    assert!(arguments.iter().any(|argument| argument == "--workspace"));
    assert!(arguments.iter().any(|argument| argument == "--timings"));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--no-tests=fail")
    );
    assert!(!arguments.iter().any(|argument| argument == "--lib"));
    let filter = arguments.last().expect("filter argument");
    assert!(filter.starts_with("(package(=ag-git)) and not"));
    assert!(filter.contains("binary(=showcase)"));
    assert!(filter.contains("binary(=protocol_compliance_e2e)"));
    assert!(filter.contains("binary(=e2e)"));

    Ok(())
}

#[test]
fn explicit_packages_narrow_compilation_and_filter_values_remain_literal()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let filter = "package(=ag-git) and test($(touch should-not-exist))";

    // Act
    let output = fixture.run(Some(filter), Some("ag-git\t ag-forge\n"), 0)?;
    let arguments = fixture.arguments()?;

    // Assert
    assert!(output.status.success());
    assert!(!arguments.iter().any(|argument| argument == "--workspace"));
    let packages: Vec<_> = arguments
        .windows(2)
        .filter(|pair| pair[0] == "-p")
        .map(|pair| pair[1].as_str())
        .collect();
    assert_eq!(packages, ["ag-git", "ag-forge"]);
    assert!(arguments.last().expect("filter").contains(filter));
    assert!(!fixture.directory.path().join("should-not-exist").exists());

    Ok(())
}

#[test]
fn missing_filters_and_blank_package_lists_fail_before_cargo_runs() -> Result<(), Box<dyn Error>> {
    for (filter, packages) in [
        (None, None),
        (Some(""), None),
        (Some("all()"), Some(" \t\n ")),
    ] {
        // Arrange
        let fixture = Fixture::new()?;

        // Act
        let output = fixture.run(filter, packages, 0)?;

        // Assert
        assert!(!output.status.success());
        assert!(!fixture.arguments_path().exists());
    }

    Ok(())
}

#[test]
fn package_patterns_are_forwarded_without_expanding_local_paths() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let output = fixture.run(Some("all()"), Some("*"), 0)?;
    let arguments = fixture.arguments()?;

    // Assert
    assert!(output.status.success());
    assert!(arguments.windows(2).any(|pair| pair == ["-p", "*"]));

    Ok(())
}

#[test]
fn runner_failures_propagate_to_the_hook_exit_status() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let output = fixture.run(Some("all()"), Some("ag-xtask"), 42)?;

    // Assert
    assert_eq!(output.status.code(), Some(42));

    Ok(())
}
