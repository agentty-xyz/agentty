//! Version parsing, latest `npm` version lookup, and auto-update command
//! tests.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;
use tempfile::{NamedTempFile, TempPath, tempdir};

use super::{
    AGENTTY_NPM_PACKAGE, MockVersionCommandRunner, NPM_REGISTRY_LATEST_URL, PackageManager,
    RealVersionCommandRunner, SHELL_INSTALLER_DOWNLOAD_TIMEOUT, SHELL_INSTALLER_URL,
    VERSION_LOOKUP_COMMAND_TIMEOUT, VERSION_UPDATE_COMMAND_TIMEOUT, VersionCommandOutput,
    VersionError, create_temp_file_in, fetch_latest_npm_version_tag, is_newer_than_current_version,
    latest_npm_version_tag, latest_npm_version_tag_with_runner, latest_version_from_result,
    parse_npm_cli_version_response, parse_registry_latest_response, parse_version,
    run_package_update_with_runner, run_update, run_update_with_runner, version_tag,
};
use crate::infra::version::VersionCommandRunner;

const LATEST_VERSION_CHILD_ENV: &str = "AGENTTY_TEST_LATEST_VERSION_CHILD";
const SHELL_UPDATE_CHILD_ENV: &str = "AGENTTY_TEST_SHELL_UPDATE_CHILD";

#[test]
fn test_parse_version_accepts_prefixed_version() {
    // Arrange
    let version = "v1.2.3";

    // Act
    let parsed_version = parse_version(version);

    // Assert
    assert_eq!(parsed_version, Some(Version::new(1, 2, 3)));
}

#[test]
fn test_parse_version_rejects_invalid_version() {
    // Arrange
    let version = "vnext";

    // Act
    let parsed_version = parse_version(version);

    // Assert
    assert_eq!(parsed_version, None);
}

#[test]
fn test_parse_npm_cli_version_response_accepts_json_string() {
    // Arrange
    let response = "\"0.1.14\"";

    // Act
    let parsed_version = parse_npm_cli_version_response(response);

    // Assert
    assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
}

#[test]
fn test_parse_registry_latest_response_extracts_version() {
    // Arrange
    let response = r#"{"name":"agentty","version":"0.1.14"}"#;

    // Act
    let parsed_version = parse_registry_latest_response(response);

    // Assert
    assert_eq!(parsed_version, Some(Version::new(0, 1, 14)));
}

#[test]
fn test_version_tag_prefixes_semver_with_v() {
    // Arrange
    let version = Version::new(0, 1, 14);

    // Act
    let version_tag = version_tag(&version);

    // Assert
    assert_eq!(version_tag, "v0.1.14");
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_prefers_npm_cli_result() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|program, args, _, timeout| {
            assert_eq!(program, "npm");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
            assert_eq!(
                args,
                vec![
                    "view".to_string(),
                    AGENTTY_NPM_PACKAGE.to_string(),
                    "version".to_string(),
                    "--json".to_string(),
                ]
            );

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: "\"0.2.0\"".to_string(),
            })
        });

    // Act
    let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

    // Assert
    assert_eq!(latest_version_tag.expect("lookup should succeed"), "v0.2.0");
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_falls_back_to_registry_curl() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|program, args, _, timeout| {
            assert_eq!(program, "npm");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
            assert_eq!(
                args,
                vec![
                    "view".to_string(),
                    AGENTTY_NPM_PACKAGE.to_string(),
                    "version".to_string(),
                    "--json".to_string(),
                ]
            );

            Ok(VersionCommandOutput {
                status: "exit status: 1".to_string(),
                stderr: "npm unavailable".to_string(),
                success: false,
                stdout: String::new(),
            })
        });
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|program, args, _, timeout| {
            assert_eq!(program, "curl");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
            assert_eq!(
                args,
                vec!["-fsSL".to_string(), NPM_REGISTRY_LATEST_URL.to_string(),]
            );

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: r#"{"name":"agentty","version":"0.3.1"}"#.to_string(),
            })
        });

    // Act
    let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

    // Assert
    assert_eq!(
        latest_version_tag.expect("fallback should succeed"),
        "v0.3.1"
    );
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_falls_back_after_npm_timeout() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    let mut sequence = mockall::Sequence::new();
    command_runner
        .expect_run_command()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|program, _, _, timeout| {
            assert_eq!(program, "npm");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Err(VersionError::CommandTimedOut {
                command: program.to_string(),
                timeout,
            })
        });
    command_runner
        .expect_run_command()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|program, _, _, timeout| {
            assert_eq!(program, "curl");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: r#"{"version":"0.3.2"}"#.to_string(),
            })
        });

    // Act
    let latest_version_tag = fetch_latest_npm_version_tag(&command_runner).await;

    // Assert
    assert_eq!(
        latest_version_tag.expect("fallback should succeed"),
        "v0.3.2"
    );
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_preserves_fallback_failure() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(2)
        .returning(|program, _, _, timeout| {
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Ok(VersionCommandOutput {
                status: "exit status: 7".to_string(),
                stderr: format!("{program} unavailable"),
                success: false,
                stdout: String::new(),
            })
        });

    // Act
    let error = fetch_latest_npm_version_tag(&command_runner)
        .await
        .expect_err("both lookup commands should fail");

    // Assert
    assert!(matches!(
        error,
        VersionError::NonZeroExit {
            command,
            status,
            stderr,
        } if command == "curl"
            && status == "exit status: 7"
            && stderr == "curl unavailable"
    ));
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_reports_invalid_fallback_response() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(2)
        .returning(|_, _, _, timeout| {
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: "not-json".to_string(),
            })
        });

    // Act
    let error = fetch_latest_npm_version_tag(&command_runner)
        .await
        .expect_err("invalid fallback response should fail");

    // Assert
    assert!(matches!(
        error,
        VersionError::ResponseParse {
            provider: "npm registry"
        }
    ));
}

#[tokio::test]
async fn test_latest_npm_version_tag_uses_injected_runner() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|program, _, _, timeout| {
            assert_eq!(program, "npm");
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: "\"0.5.0\"".to_string(),
            })
        });

    // Act
    let version_tag = latest_npm_version_tag_with_runner(&command_runner).await;

    // Assert
    assert_eq!(version_tag.as_deref(), Some("v0.5.0"));
}

#[tokio::test]
async fn test_latest_npm_version_tag_uses_isolated_real_runner() {
    if std::env::var_os(LATEST_VERSION_CHILD_ENV).is_some() {
        // Arrange
        let expected_version_tag = "v0.6.0";

        // Act
        let version_tag = latest_npm_version_tag().await;

        // Assert
        assert_eq!(version_tag.as_deref(), Some(expected_version_tag));

        return;
    }

    // Arrange
    let command_dir = tempdir().expect("failed to create fake command directory");
    let npm_path = command_dir.path().join("npm");
    std::fs::write(&npm_path, "#!/bin/sh\nprintf '\"0.6.0\"'\n")
        .expect("failed to write fake npm command");
    let mut permissions = std::fs::metadata(&npm_path)
        .expect("failed to load fake npm metadata")
        .permissions();
    permissions.set_mode(0o750);
    std::fs::set_permissions(&npm_path, permissions).expect("failed to make fake npm executable");
    let current_test_binary =
        std::env::current_exe().expect("failed to resolve current test binary");

    // Act
    let output = tokio::process::Command::new(current_test_binary)
        .arg("--exact")
        .arg("infra::version::tests::test_latest_npm_version_tag_uses_isolated_real_runner")
        .arg("--nocapture")
        .env("PATH", command_dir.path())
        .env(LATEST_VERSION_CHILD_ENV, "1")
        .output()
        .await
        .expect("failed to run isolated latest-version test");

    // Assert
    assert!(
        output.status.success(),
        "isolated latest-version test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn test_fetch_latest_npm_version_tag_preserves_command_errors() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(2)
        .returning(|program, _, _, timeout| {
            assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);

            Err(VersionError::CommandSpawn {
                command: program.to_string(),
                message: "not installed".to_string(),
            })
        });

    // Act
    let error = fetch_latest_npm_version_tag(&command_runner)
        .await
        .expect_err("fallback command error should propagate");

    // Assert
    assert!(matches!(
        error,
        VersionError::CommandSpawn { command, message }
            if command == "curl" && message == "not installed"
    ));
}

#[tokio::test]
async fn test_real_version_command_runner_captures_process_output() {
    // Arrange
    let command_runner = RealVersionCommandRunner;

    // Act
    let output = command_runner
        .run_command(
            "sh",
            vec![
                "-c".to_string(),
                "printf '0.4.0 %s' \"$AGENTTY_TEST_EXTRA\"; printf 'notice' >&2".to_string(),
            ],
            vec![("AGENTTY_TEST_EXTRA".to_string(), "env".to_string())],
            Duration::from_secs(1),
        )
        .await
        .expect("command should run");

    // Assert
    assert!(output.success);
    assert_eq!(output.stdout, "0.4.0 env");
    assert_eq!(output.stderr, "notice");
    assert!(output.status.contains('0'));
}

#[tokio::test]
async fn test_real_version_command_runner_reports_spawn_failure() {
    // Arrange
    let command_runner = RealVersionCommandRunner;

    // Act
    let error = command_runner
        .run_command(
            "agentty-command-that-does-not-exist",
            Vec::new(),
            Vec::new(),
            Duration::from_secs(1),
        )
        .await
        .expect_err("missing command should fail");

    // Assert
    assert!(matches!(
        error,
        VersionError::CommandSpawn { command, message }
            if command == "agentty-command-that-does-not-exist" && !message.is_empty()
    ));
}

#[tokio::test]
async fn test_real_version_command_runner_cancels_timed_out_process() {
    // Arrange
    let command_runner = RealVersionCommandRunner;
    let timeout = Duration::from_millis(25);

    // Act
    let error = command_runner
        .run_command(
            "sh",
            vec!["-c".to_string(), "exec sleep 5".to_string()],
            Vec::new(),
            timeout,
        )
        .await
        .expect_err("long-running command should time out");

    // Assert
    assert!(matches!(
        error,
        VersionError::CommandTimedOut {
            command,
            timeout: actual_timeout,
        } if command == "sh" && actual_timeout == timeout
    ));
}

/// A timed-out shell script receives `SIGTERM` with its children, so its
/// trap runs promptly instead of after its foreground `sleep`.
#[tokio::test]
async fn test_real_version_command_runner_terminates_timed_out_script_group() {
    // Arrange
    let directory = tempdir().expect("fixture directory");
    let script = directory.path().join("script.sh");
    let marker = directory.path().join("terminated");
    std::fs::write(
        &script,
        "trap 'printf terminated > \"$AGENTTY_TEST_MARKER.tmp\"; mv \"$AGENTTY_TEST_MARKER.tmp\" \
         \"$AGENTTY_TEST_MARKER\"; exit 1' TERM\nsleep 30\n",
    )
    .expect("fixture script");
    let command_runner = RealVersionCommandRunner;
    // Long enough for `sh` to install its trap before the deadline.
    let timeout = Duration::from_millis(300);

    // Act
    let error = command_runner
        .run_shell_script(
            script.to_str().expect("UTF-8 path"),
            vec![(
                "AGENTTY_TEST_MARKER".to_string(),
                marker.to_string_lossy().into_owned(),
            )],
            timeout,
        )
        .await
        .expect_err("long-running script should time out");
    let trap_ran = tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    // Assert
    assert!(matches!(
        error,
        VersionError::CommandTimedOut {
            command,
            timeout: actual_timeout,
        } if command == "sh" && actual_timeout == timeout
    ));
    assert!(trap_ran.is_ok(), "TERM trap did not run");
    assert_eq!(
        std::fs::read_to_string(&marker).expect("marker"),
        "terminated"
    );
}

#[tokio::test]
async fn test_latest_version_result_preserves_optional_public_contract() {
    // Arrange
    let success = Ok("v1.2.3".to_string());
    let lookup_failure = Err(VersionError::ResponseParse { provider: "npm" });

    // Act
    let successful_version = latest_version_from_result(success);
    let missing_version = latest_version_from_result(lookup_failure);

    // Assert
    assert_eq!(successful_version.as_deref(), Some("v1.2.3"));
    assert_eq!(missing_version, None);
}

#[test]
fn test_is_newer_than_current_version_returns_true_when_candidate_is_newer() {
    // Arrange
    let current_version = "0.1.11";
    let candidate_version = "v0.1.12";

    // Act
    let is_newer = is_newer_than_current_version(current_version, candidate_version);

    // Assert
    assert!(is_newer);
}

#[test]
fn test_is_newer_than_current_version_returns_false_when_candidate_is_not_newer() {
    // Arrange
    let current_version = "0.1.12";
    let candidate_version = "v0.1.11";

    // Act
    let is_newer = is_newer_than_current_version(current_version, candidate_version);

    // Assert
    assert!(!is_newer);
}

#[test]
fn test_is_newer_than_current_version_rejects_invalid_versions() {
    // Arrange, Act
    let invalid_current = is_newer_than_current_version("current", "v1.0.0");
    let invalid_candidate = is_newer_than_current_version("1.0.0", "candidate");

    // Assert
    assert!(!invalid_current);
    assert!(!invalid_candidate);
}

#[tokio::test]
async fn test_run_npm_update_calls_npm_install_global() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|command, args, envs, timeout| {
            assert_eq!(envs, Vec::<(String, String)>::new());
            assert_eq!(command, "npm");
            assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);
            assert_eq!(
                args,
                vec![
                    "i".to_string(),
                    "-g".to_string(),
                    "agentty@latest".to_string(),
                ]
            );

            Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                success: true,
                stdout: "added 1 package".to_string(),
            })
        });

    // Act
    let output = run_package_update_with_runner(PackageManager::Npm, &command_runner)
        .await
        .expect("update should succeed");

    // Assert
    assert_eq!(output, "added 1 package");
}

#[tokio::test]
async fn test_run_npm_update_preserves_runner_error_without_displaying_stderr() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|_, _, _, timeout| {
            assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);

            Ok(VersionCommandOutput {
                status: "exit status: 1".to_string(),
                stderr: "permission denied".to_string(),
                success: false,
                stdout: String::new(),
            })
        });

    // Act
    let error = run_package_update_with_runner(PackageManager::Npm, &command_runner)
        .await
        .expect_err("should propagate runner error");

    // Assert
    assert_eq!(error.to_string(), "`npm` exited with status exit status: 1");
    assert!(matches!(
        error,
        VersionError::NonZeroExit { stderr, .. } if stderr == "permission denied"
    ));
}

#[tokio::test]
async fn test_run_npm_update_preserves_timeout() {
    // Arrange
    let mut command_runner = MockVersionCommandRunner::new();
    command_runner
        .expect_run_command()
        .times(1)
        .returning(|command, _, _, timeout| {
            Err(VersionError::CommandTimedOut {
                command: command.to_string(),
                timeout,
            })
        });

    // Act
    let error = run_package_update_with_runner(PackageManager::Npm, &command_runner)
        .await
        .expect_err("timed-out update should fail");

    // Assert
    assert!(matches!(
        error,
        VersionError::CommandTimedOut {
            command,
            timeout,
        } if command == "npm" && timeout == VERSION_UPDATE_COMMAND_TIMEOUT
    ));
}

/// Resolves realistic installation links and records only the selected update.
#[tokio::test]
async fn updates_only_the_matching_global_manager() {
    // Arrange
    for selected in [
        PackageManager::Npm,
        PackageManager::Bun,
        PackageManager::Pnpm,
    ] {
        let mut runner = MockVersionCommandRunner::new();
        runner.expect_canonicalize_path().returning(move |path| {
            if path == Path::new("/running/agentty") {
                return Some(path.to_path_buf());
            }
            if path == Path::new("/bun/agentty") {
                return Some(PathBuf::from("/bun/package/run-agentty.js"));
            }
            let candidate = match selected {
                PackageManager::Npm => "/npm/agentty/node_modules/.bin_real/agentty",
                PackageManager::Bun => "/bun/package/node_modules/.bin_real/agentty",
                PackageManager::Pnpm => "/pnpm/agentty/node_modules/.bin_real/agentty",
            };

            (path == Path::new(candidate)).then(|| PathBuf::from("/running/agentty"))
        });
        runner
            .expect_run_command()
            .times(4)
            .returning(move |program, args, _, timeout| {
                let manager = [
                    PackageManager::Npm,
                    PackageManager::Bun,
                    PackageManager::Pnpm,
                ]
                .into_iter()
                .find(|manager| manager.program() == program)
                .expect("unexpected program");
                let stdout = if timeout == VERSION_LOOKUP_COMMAND_TIMEOUT {
                    let expected = match manager {
                        PackageManager::Npm => vec!["root", "--global"],
                        PackageManager::Bun => vec!["pm", "bin", "--global"],
                        PackageManager::Pnpm => vec!["list", "--global", "--depth", "0", "--json"],
                    };
                    assert_eq!(args, expected);
                    if manager == PackageManager::Pnpm {
                        r#"[{"dependencies":{"agentty":{"path":"/pnpm/agentty"}}}]"#.to_string()
                    } else {
                        format!("/{program}\n")
                    }
                } else {
                    assert_eq!(manager, selected);
                    assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);
                    let expected = match selected {
                        PackageManager::Npm => vec!["i", "-g", "agentty@latest"],
                        PackageManager::Bun | PackageManager::Pnpm => {
                            vec!["add", "-g", "agentty@latest"]
                        }
                    };
                    assert_eq!(args, expected);
                    "updated".to_string()
                };

                Ok(VersionCommandOutput {
                    status: "0".to_string(),
                    stderr: String::new(),
                    stdout,
                    success: true,
                })
            });

        // A package-manager match wins without consulting a shell receipt.
        runner.expect_read_file().times(0);

        // Act
        let result = run_update_with_runner(
            Some(Path::new("/running/agentty")),
            Some(Path::new(RECEIPT_PATH)),
            &runner,
        )
        .await;

        // Assert
        assert_eq!(result.expect("selected installer should update"), "updated");
    }
}

#[tokio::test]
async fn missing_executable_requires_manual_update_without_commands() {
    // Arrange
    let mut runner = MockVersionCommandRunner::new();
    runner.expect_canonicalize_path().return_const(None);
    runner.expect_read_file().times(0);
    runner.expect_run_command().times(0);

    // Act
    let missing = run_update_with_runner(None, Some(Path::new(RECEIPT_PATH)), &runner).await;
    let unresolved = run_update_with_runner(
        Some(Path::new("/missing")),
        Some(Path::new(RECEIPT_PATH)),
        &runner,
    )
    .await;

    // Assert
    assert!(matches!(&missing, Err(VersionError::UnknownInstallation)));
    let hint = missing.expect_err("manual update required").to_string();
    assert!(hint.contains("shell installer"), "{hint}");
    assert!(hint.contains("cargo install agentty --locked"), "{hint}");
    assert!(hint.contains("npx agentty@latest"), "{hint}");
    assert!(matches!(unresolved, Err(VersionError::UnknownInstallation)));
}

#[tokio::test]
async fn uncertain_installations_never_run_an_install_command() {
    // Arrange
    for case in [
        "unmatched",
        "ambiguous",
        "spawn",
        "exit",
        "relative",
        "missing_launcher",
        "root_launcher",
        "empty_pnpm",
        "relative_pnpm",
    ] {
        let mut runner = MockVersionCommandRunner::new();
        runner.expect_canonicalize_path().returning(move |path| {
            if path == Path::new("/running/agentty") {
                return Some(path.to_path_buf());
            }
            match case {
                "ambiguous" => Some(PathBuf::from("/running/agentty")),
                "root_launcher" if path == Path::new("/global/agentty") => Some(PathBuf::from("/")),
                _ => None,
            }
        });
        runner
            .expect_run_command()
            .times(3)
            .returning(move |program, args, _, timeout| {
                assert_eq!(timeout, VERSION_LOOKUP_COMMAND_TIMEOUT);
                assert!(args.contains(&"--global".to_string()));
                assert!(!args.contains(&"agentty@latest".to_string()));
                if case == "spawn" {
                    return Err(VersionError::CommandSpawn {
                        command: program.to_string(),
                        message: "missing".to_string(),
                    });
                }

                Ok(VersionCommandOutput {
                    status: "1".to_string(),
                    stderr: String::new(),
                    stdout: if case == "relative" {
                        "relative"
                    } else if program == "pnpm" && case == "ambiguous" {
                        r#"[{"dependencies":{"agentty":{"path":"/global"}}}]"#
                    } else if program == "pnpm" && case == "empty_pnpm" {
                        "[{}]"
                    } else if program == "pnpm" && case == "relative_pnpm" {
                        r#"[{"dependencies":{"agentty":{"path":"relative"}}}]"#
                    } else {
                        "/global"
                    }
                    .to_string(),
                    success: case != "exit",
                })
            });

        // Act
        let result =
            run_update_with_runner(Some(Path::new("/running/agentty")), None, &runner).await;

        // Assert
        assert!(
            matches!(result, Err(VersionError::UnknownInstallation)),
            "{case}"
        );
    }
}

#[test]
fn real_runner_resolves_existing_paths_and_rejects_missing_paths() {
    // Arrange
    let directory = tempdir().expect("fixture directory");
    let runner = RealVersionCommandRunner;

    // Act
    let existing = runner.canonicalize_path(directory.path());
    let missing = runner.canonicalize_path(&directory.path().join("missing"));

    // Assert
    assert_eq!(
        existing,
        Some(directory.path().canonicalize().expect("canonical path"))
    );
    assert_eq!(missing, None);
}

#[test]
fn real_runner_reads_existing_files_and_rejects_missing_files() {
    // Arrange
    let directory = tempdir().expect("fixture directory");
    let file = directory.path().join("receipt.json");
    std::fs::write(&file, b"receipt").expect("fixture file");
    let runner = RealVersionCommandRunner;

    // Act
    let existing = runner.read_file(&file);
    let missing = runner.read_file(&directory.path().join("missing"));

    // Assert
    assert_eq!(existing.as_deref(), Some(b"receipt".as_slice()));
    assert_eq!(missing, None);
}

#[test]
fn real_runner_stages_private_temp_files_removed_on_drop() {
    // Arrange
    let directory = tempdir().expect("fixture directory");
    let runner = RealVersionCommandRunner;

    // Act
    let staged = runner.create_temp_file().expect("temporary file");
    let staged_path = staged.to_path_buf();
    let metadata = std::fs::metadata(&staged_path).expect("staged file metadata");
    drop(staged);
    let error = create_temp_file_in(&directory.path().join("missing"))
        .expect_err("missing directory should fail");

    // Assert
    assert_eq!(metadata.len(), 0);
    assert_eq!(metadata.permissions().mode() & 0o077, 0);
    assert!(!staged_path.exists());
    assert!(matches!(error, VersionError::InstallerStaging { message } if !message.is_empty()));
}

/// Receipt location the shell-update tests pass to the updater.
const RECEIPT_PATH: &str = "/config/agentty/agentty-receipt.json";

/// Scripted result of one shell-update command.
#[derive(Clone, Copy, Debug)]
enum Step {
    Succeed,
    Fail,
    TimeOut,
}

impl Step {
    /// Returns the scripted command result for `program`.
    fn output(
        self,
        program: &str,
        timeout: Duration,
    ) -> Result<VersionCommandOutput, VersionError> {
        match self {
            Self::Succeed => Ok(VersionCommandOutput {
                status: "exit status: 0".to_string(),
                stderr: String::new(),
                stdout: format!("{program} done"),
                success: true,
            }),
            Self::Fail => Ok(VersionCommandOutput {
                status: "exit status: 1".to_string(),
                stderr: format!("{program} failed"),
                stdout: String::new(),
                success: false,
            }),
            Self::TimeOut => Err(VersionError::CommandTimedOut {
                command: program.to_string(),
                timeout,
            }),
        }
    }
}

/// Returns a receipt the shell installer writes for `version` at `/prefix`.
fn shell_receipt(version: &str) -> Vec<u8> {
    serde_json::json!({
        "binaries": ["agentty"],
        "install_layout": "cargo-home",
        "install_prefix": "/prefix",
        "modify_path": false,
        "provider": { "source": "ag-release", "version": "0.1.0" },
        "source": { "app_name": "agentty", "name": "agentty", "owner": "agentty-xyz" },
        "version": version,
    })
    .to_string()
    .into_bytes()
}

/// Returns a runner for a running binary at `/running/agentty` that no
/// package manager installed, `/prefix/bin/agentty` resolving to it, and
/// `receipt` at [`RECEIPT_PATH`].
fn shell_install_runner(receipt: Option<Vec<u8>>) -> MockVersionCommandRunner {
    let mut runner = MockVersionCommandRunner::new();
    runner.expect_canonicalize_path().returning(|path| {
        [
            Path::new("/running/agentty"),
            Path::new("/prefix/bin/agentty"),
        ]
        .contains(&path)
        .then(|| PathBuf::from("/running/agentty"))
    });
    runner.expect_read_file().times(1).returning(move |path| {
        assert_eq!(path, Path::new(RECEIPT_PATH));

        receipt.clone()
    });

    runner
}

/// Returns whether `program` is a package manager whose discovery fails.
fn is_unavailable_package_manager(program: &str) -> bool {
    ["npm", "bun", "pnpm"].contains(&program)
}

/// Downloads the installer to a temporary file, runs it with `sh` and the
/// receipt's prefix, reports each failure, and always removes the file.
#[tokio::test]
async fn shell_install_reruns_downloaded_installer_and_removes_it() {
    for (download, install) in [
        (Step::Succeed, Step::Succeed),
        (Step::Fail, Step::Succeed),
        (Step::TimeOut, Step::Succeed),
        (Step::Succeed, Step::Fail),
        (Step::Succeed, Step::TimeOut),
    ] {
        // Arrange
        let case = format!("{download:?}, {install:?}");
        let directory = tempdir().expect("fixture directory");
        let installer = NamedTempFile::new_in(directory.path())
            .expect("installer file")
            .into_temp_path();
        let installer_path = installer.to_str().expect("UTF-8 path").to_string();
        let mut runner = shell_install_runner(Some(shell_receipt(env!("CARGO_PKG_VERSION"))));
        runner
            .expect_create_temp_file()
            .times(1)
            .return_once(move || Ok(installer));
        let expected_path = installer_path.clone();
        runner
            .expect_run_command()
            .returning(move |program, args, envs, timeout| {
                if is_unavailable_package_manager(program) {
                    return Err(VersionError::CommandSpawn {
                        command: program.to_string(),
                        message: "not installed".to_string(),
                    });
                }
                assert_eq!(program, "curl");
                assert_eq!(
                    args,
                    [
                        "--proto",
                        "=https",
                        "--tlsv1.2",
                        "--fail",
                        "--location",
                        "--silent",
                        "--show-error",
                        "--output",
                        expected_path.as_str(),
                        SHELL_INSTALLER_URL,
                    ]
                );
                assert_eq!(envs, Vec::<(String, String)>::new());
                assert_eq!(timeout, SHELL_INSTALLER_DOWNLOAD_TIMEOUT);

                download.output(program, timeout)
            });
        let expected_path = installer_path.clone();
        // The installer runs only after a successful download.
        runner
            .expect_run_shell_script()
            .times(usize::from(matches!(download, Step::Succeed)))
            .returning(move |script, envs, timeout| {
                assert_eq!(script, expected_path);
                assert_eq!(
                    envs,
                    [
                        ("AGENTTY_INSTALL_DIR".to_string(), "/prefix".to_string()),
                        ("AGENTTY_NO_MODIFY_PATH".to_string(), "1".to_string()),
                        ("AGENTTY_UNMANAGED_INSTALL".to_string(), String::new()),
                    ]
                );
                assert_eq!(timeout, VERSION_UPDATE_COMMAND_TIMEOUT);

                install.output("sh", timeout)
            });

        // Act
        let result = run_update_with_runner(
            Some(Path::new("/running/agentty")),
            Some(Path::new(RECEIPT_PATH)),
            &runner,
        )
        .await;

        // Assert
        assert!(!Path::new(&installer_path).exists(), "{case}");
        match (download, install) {
            (Step::Succeed, Step::Succeed) => {
                assert_eq!(result.expect("shell update").as_str(), "sh done");
            }
            (Step::Fail, _) => assert!(
                matches!(result, Err(VersionError::NonZeroExit { command, stderr, .. })
                    if command == "curl" && stderr == "curl failed"),
                "{case}"
            ),
            (Step::TimeOut, _) => assert!(
                matches!(result, Err(VersionError::CommandTimedOut { command, timeout })
                    if command == "curl" && timeout == SHELL_INSTALLER_DOWNLOAD_TIMEOUT),
                "{case}"
            ),
            (Step::Succeed, Step::Fail) => assert!(
                matches!(result, Err(VersionError::NonZeroExit { command, stderr, .. })
                    if command == "sh" && stderr == "sh failed"),
                "{case}"
            ),
            (Step::Succeed, Step::TimeOut) => assert!(
                matches!(result, Err(VersionError::CommandTimedOut { command, timeout })
                    if command == "sh" && timeout == VERSION_UPDATE_COMMAND_TIMEOUT),
                "{case}"
            ),
        }
    }
}

/// Requires a manual update, downloading nothing, unless the receipt proves
/// the running path and names this build or a newer one.
#[tokio::test]
async fn shell_update_requires_a_receipt_for_the_running_binary() {
    let other_prefix = String::from_utf8(shell_receipt(env!("CARGO_PKG_VERSION")))
        .expect("UTF-8 receipt")
        .replace("\"/prefix\"", "\"/other\"")
        .into_bytes();
    for (case, receipt) in [
        ("missing", None),
        ("unreadable", Some(b"not json".to_vec())),
        ("old version", Some(shell_receipt("0.0.0-old"))),
        ("other prefix", Some(other_prefix)),
    ] {
        // Arrange
        let mut runner = shell_install_runner(receipt);
        runner.expect_create_temp_file().times(0);
        runner
            .expect_run_command()
            .times(3)
            .returning(|program, _, _, _| {
                assert!(is_unavailable_package_manager(program), "{program} ran");

                Err(VersionError::CommandSpawn {
                    command: program.to_string(),
                    message: "not installed".to_string(),
                })
            });

        // Act
        let result = run_update_with_runner(
            Some(Path::new("/running/agentty")),
            Some(Path::new(RECEIPT_PATH)),
            &runner,
        )
        .await;

        // Assert
        assert!(
            matches!(result, Err(VersionError::UnknownInstallation)),
            "{case}"
        );
    }
}

/// Reruns the installer when another process already updated the install,
/// leaving a receipt newer than the running build.
#[tokio::test]
async fn shell_update_runs_for_a_receipt_newer_than_the_running_build() {
    // Arrange
    let running = Version::parse(env!("CARGO_PKG_VERSION")).expect("running version");
    let newer = Version::new(running.major, running.minor + 1, 0).to_string();
    let directory = tempdir().expect("fixture directory");
    let installer = NamedTempFile::new_in(directory.path())
        .expect("installer file")
        .into_temp_path();
    let mut runner = shell_install_runner(Some(shell_receipt(&newer)));
    runner
        .expect_create_temp_file()
        .times(1)
        .return_once(move || Ok(installer));
    runner
        .expect_run_command()
        .times(4)
        .returning(|program, _, _, timeout| {
            if is_unavailable_package_manager(program) {
                return Err(VersionError::CommandSpawn {
                    command: program.to_string(),
                    message: "not installed".to_string(),
                });
            }

            Step::Succeed.output(program, timeout)
        });
    runner
        .expect_run_shell_script()
        .times(1)
        .returning(|_, _, timeout| Step::Succeed.output("sh", timeout));

    // Act
    let result = run_update_with_runner(
        Some(Path::new("/running/agentty")),
        Some(Path::new(RECEIPT_PATH)),
        &runner,
    )
    .await;

    // Assert
    assert_eq!(result.expect("shell update").as_str(), "sh done");
}

/// Stops before downloading when the installer cannot be staged.
#[tokio::test]
async fn shell_update_requires_a_usable_staging_file() {
    for case in ["create", "non-UTF-8"] {
        // Arrange
        let mut runner = shell_install_runner(Some(shell_receipt(env!("CARGO_PKG_VERSION"))));
        runner
            .expect_create_temp_file()
            .times(1)
            .returning(move || {
                if case == "create" {
                    return Err(VersionError::InstallerStaging {
                        message: "disk full".to_string(),
                    });
                }

                Ok(TempPath::try_from_path(PathBuf::from(OsString::from_vec(
                    b"/nonexistent/agentty-\xff.sh".to_vec(),
                )))
                .expect("absolute temporary path"))
            });
        runner
            .expect_run_command()
            .times(3)
            .returning(|program, _, _, _| {
                assert!(is_unavailable_package_manager(program), "{program} ran");

                Err(VersionError::CommandSpawn {
                    command: program.to_string(),
                    message: "not installed".to_string(),
                })
            });

        // Act
        let result = run_update_with_runner(
            Some(Path::new("/running/agentty")),
            Some(Path::new(RECEIPT_PATH)),
            &runner,
        )
        .await;

        // Assert
        assert!(
            matches!(result, Err(VersionError::InstallerStaging { .. })),
            "{case}"
        );
    }
}

/// Runs the shell update with real processes: a `curl` stub serves a fake
/// installer, real `sh` runs it, and the staged script is removed afterwards.
#[tokio::test]
async fn shell_update_runs_downloaded_installer_with_real_commands() {
    if std::env::var_os(SHELL_UPDATE_CHILD_ENV).is_some() {
        // Act
        let output = run_update().await;

        // Assert
        assert_eq!(output.expect("shell update").as_str(), "installed");

        return;
    }

    // Arrange
    let root = tempdir().expect("fixture root");
    let root_path = root.path().canonicalize().expect("canonical fixture root");
    let command_dir = root_path.join("commands");
    let prefix = root_path.join("prefix");
    let receipt_dir = root_path.join("config/agentty");
    for directory in [&command_dir, &prefix.join("bin"), &receipt_dir] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }
    let current_test_binary =
        std::env::current_exe().expect("failed to resolve current test binary");
    std::os::unix::fs::symlink(&current_test_binary, prefix.join("bin/agentty"))
        .expect("installed binary link");
    std::os::unix::fs::symlink("/bin/sh", command_dir.join("sh")).expect("sh link");
    let receipt = String::from_utf8(shell_receipt(env!("CARGO_PKG_VERSION")))
        .expect("UTF-8 receipt")
        .replace(
            "\"/prefix\"",
            &serde_json::to_string(&prefix).expect("prefix JSON"),
        );
    std::fs::write(receipt_dir.join("agentty-receipt.json"), receipt).expect("receipt");
    std::fs::write(
        root_path.join("installer.sh"),
        "printf '%s\\n' \"$AGENTTY_INSTALL_DIR\" \"$AGENTTY_NO_MODIFY_PATH\" \
         \"${AGENTTY_UNMANAGED_INSTALL-unset}\" \"$0\" > \
         \"$AGENTTY_TEST_ROOT/installer-run\"\nprintf installed\n",
    )
    .expect("fake installer");
    let curl_path = command_dir.join("curl");
    std::fs::write(
        &curl_path,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AGENTTY_TEST_ROOT/curl-args\"\nwhile [ \"$#\" -gt \
         0 ]; do\n  if [ \"$1\" = --output ]; then output=$2; fi\n  shift\ndone\n/bin/cat \
         \"$AGENTTY_TEST_ROOT/installer.sh\" > \"$output\"\n",
    )
    .expect("fake curl");
    std::fs::set_permissions(&curl_path, std::fs::Permissions::from_mode(0o700))
        .expect("executable fake curl");

    // Act
    let output = tokio::process::Command::new(current_test_binary)
        .arg("--exact")
        .arg("infra::version::tests::shell_update_runs_downloaded_installer_with_real_commands")
        .arg("--nocapture")
        .env("PATH", &command_dir)
        .env("HOME", root_path.join("home"))
        .env("XDG_CONFIG_HOME", root_path.join("config"))
        .env("AGENTTY_TEST_ROOT", &root_path)
        .env(SHELL_UPDATE_CHILD_ENV, "1")
        .output()
        .await
        .expect("failed to run isolated shell-update test");

    // Assert
    assert!(
        output.status.success(),
        "isolated shell-update test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installer_run =
        std::fs::read_to_string(root_path.join("installer-run")).expect("installer ran");
    let installer_run = installer_run.lines().collect::<Vec<_>>();
    let prefix = prefix.to_string_lossy();
    assert_eq!(installer_run[..3], [prefix.as_ref(), "1", ""]);
    assert!(
        !Path::new(installer_run[3]).exists(),
        "staged installer kept"
    );
    let curl_args = std::fs::read_to_string(root_path.join("curl-args")).expect("curl ran");
    assert_eq!(curl_args.lines().last(), Some(SHELL_INSTALLER_URL));
    assert!(curl_args.lines().any(|arg| arg == installer_run[3]));
}
