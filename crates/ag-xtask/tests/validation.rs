//! Public coordinated-validation CLI contracts with injected executables.

#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_yaml_ng::Value;
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path();
        fs::write(path.join("source.rs"), "first version")?;
        fs::write(
            path.join("metadata.json"),
            r#"{"target_directory":"target","workspace_members":["core","app","other"],"packages":[{"id":"core","name":"core","manifest_path":"/workspace/core/Cargo.toml","dependencies":[]},{"id":"app","name":"app","manifest_path":"/workspace/app/Cargo.toml","dependencies":[{"name":"core","path":"core"}]},{"id":"other","name":"other","manifest_path":"/workspace/other/Cargo.toml","dependencies":[]}]}"#,
        )?;
        fs::write(
            path.join(".pre-commit-config.yaml"),
            "default_stages: [pre-commit]\nrepos:\n- hooks:\n  - id: clippy\n  - id: \
             test-focused\n    stages: [manual]\n  - id: coverage\n    stages: [manual]\n",
        )?;
        for (name, script) in [
            (
                "cargo",
                r#"if [ "$1" = metadata ]; then
                    sed "s|\"target_directory\":\"target\"|\"target_directory\":\"${CARGO_TARGET_DIR:-target}\"|" metadata.json
                else echo nextest-version; fi"#,
            ),
            ("rustc", "echo rustc-version"),
            (
                "git",
                "if [ \"$1\" = ls-files ]; then printf \
                 'source.rs\\0deleted.rs\\0linked-inputs\\0'; else echo git-state; fi",
            ),
            (
                "prek",
                "if [ \"$1\" = --version ]; then echo prek-version; else printf '%s|%s|%s\\n' \
                 \"$*\" \"$AGENTTY_TEST_PACKAGES\" \"$AGENTTY_TEST_FILTER\" >> calls; exit \
                 \"${CHECK_EXIT:-0}\"; fi",
            ),
        ] {
            let executable = path.join(name);
            fs::write(&executable, format!("#!/bin/sh\n{script}\n"))?;
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
        }

        Ok(Self { directory })
    }

    fn run(&self, arguments: &[&str], exit_code: i32) -> std::io::Result<Output> {
        self.run_in_target(arguments, exit_code, "target")
    }

    fn run_in_target(
        &self,
        arguments: &[&str],
        exit_code: i32,
        target: &str,
    ) -> std::io::Result<Output> {
        self.command(arguments, exit_code)
            .env("CARGO_TARGET_DIR", target)
            .output()
    }

    fn command(&self, arguments: &[&str], exit_code: i32) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ag-xtask"));
        command
            .arg("validate")
            .args(arguments)
            .current_dir(self.directory.path())
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.directory.path().display()),
            )
            .env("AGENTTY_TEST_PACKAGES", "core")
            .env("CHECK_EXIT", exit_code.to_string());
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }

        command
    }

    fn configure_cargo_target(&self) -> Result<(), Box<dyn Error>> {
        let path = self.directory.path();
        fs::create_dir(path.join("src"))?;
        fs::create_dir(path.join(".cargo"))?;
        fs::write(path.join("src/lib.rs"), "")?;
        fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"core\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n",
        )?;
        fs::write(
            path.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"core\"\nversion = \"0.0.0\"\n",
        )?;
        fs::write(
            path.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"config-cache\"\n",
        )?;
        fs::write(
            path.join("cargo"),
            r#"#!/bin/sh
if [ "$1" = metadata ]; then "$AGENTTY_REAL_CARGO" "$@"; else echo nextest-version; fi
"#,
        )?;
        fs::write(
            path.join("git"),
            r#"#!/bin/sh
excluded=false
others=false
for argument in "$@"; do
    if [ "$argument" = ":(top,literal,exclude)$EXPECTED_TARGET" ]; then excluded=true; fi
    if [ "$argument" = --others ]; then others=true; fi
done
case "$1" in
    ls-files)
        printf 'source.rs\0'
        if "$others" && ! "$excluded" && [ -d "$EXPECTED_TARGET" ]; then
            find "$EXPECTED_TARGET" -type f -print0
        fi
        ;;
    status)
        if ! "$excluded" && [ -d "$EXPECTED_TARGET" ]; then
            find "$EXPECTED_TARGET" -type f -exec printf '?? %s\0' {} \;
        fi
        ;;
    *) echo git-state ;;
esac
exit 0
"#,
        )?;
        fs::write(
            path.join("prek"),
            r#"#!/bin/sh
if [ "$1" = --version ]; then echo prek-version; else
    echo "$*|$CARGO_TARGET_DIR" >> calls
    output="${CARGO_TARGET_DIR:-$EXPECTED_TARGET}"
    mkdir -p "$output"
    echo generated >> "$output/generated"
fi
"#,
        )?;

        Ok(())
    }

    fn calls(&self) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(fs::read_to_string(self.directory.path().join("calls"))?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

#[test]
fn cargo_configuration_selects_the_directory_for_outputs_coordination_and_reuse()
-> Result<(), Box<dyn Error>> {
    // Arrange
    for (variable, target) in [
        (None, "config-cache"),
        (Some("CARGO_BUILD_TARGET_DIR"), "env-cache"),
        (Some("CARGO_TARGET_DIR"), "explicit-cache"),
    ] {
        let fixture = Fixture::new()?;
        fixture.configure_cargo_target()?;
        let mut outputs = Vec::new();
        // Act
        for arguments in [vec![], vec!["--reuse"]] {
            let mut command = fixture.command(&arguments, 0);
            command
                .env("AGENTTY_REAL_CARGO", env!("CARGO"))
                .env("RUSTC", Path::new(env!("CARGO")).with_file_name("rustc"))
                .env("RUSTC_WRAPPER", "")
                .env("CARGO_HOME", fixture.directory.path().join("cargo-home"))
                .env("EXPECTED_TARGET", target);
            for name in ["RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "RUSTUP_AUTO_INSTALL"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            if let Some(variable) = variable {
                command.env(variable, target);
            }
            outputs.push(command.output()?);
        }
        // Assert
        for output in outputs {
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let path = fixture.directory.path();
        assert!(path.join(target).join("agentty-validation.lock").exists());
        assert!(path.join(target).join("agentty-validation.json").exists());
        assert!(path.join(target).join("generated").exists());
        assert!(!path.join("target").exists());
        let calls = fixture.calls()?;
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| call.ends_with(&format!("|{target}")))
        );
    }

    Ok(())
}

#[test]
fn metadata_targets_outside_the_workspace_are_rejected_before_creating_outputs()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let path = fixture.directory.path();
    let metadata = fs::read_to_string(path.join("metadata.json"))?.replace(
        "\"target_directory\":\"target\"",
        "\"target_directory\":\"/outside\"",
    );
    fs::write(path.join("metadata.json"), metadata)?;
    // Act
    let output = fixture.command(&[], 0).output()?;
    // Assert
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("within the workspace"));
    assert!(!path.join("target").exists());
    assert!(!path.join("calls").exists());

    Ok(())
}

#[test]
fn inherited_skip_variables_cannot_skip_checks_or_create_unearned_successes()
-> Result<(), Box<dyn Error>> {
    // Arrange
    for (skip, prek_skip) in [
        (Some("clippy,test-focused"), None),
        (None, Some("clippy,test-focused")),
        (Some("clippy"), Some("test-focused")),
    ] {
        let fixture = Fixture::new()?;
        let script = fs::read_to_string(fixture.directory.path().join("prek"))?.replacen(
            "#!/bin/sh\n",
            "#!/bin/sh\nif [ \"$1\" != --version ] && [ -n \"${SKIP}${PREK_SKIP}\" ]; then exit \
             0; fi\n",
            1,
        );
        fs::write(fixture.directory.path().join("prek"), script)?;
        // Act
        let mut outputs = Vec::new();
        for arguments in [vec![], vec!["--reuse"]] {
            let mut command = fixture.command(&arguments, 0);
            if let Some(skip) = skip {
                command.env("SKIP", skip);
            }
            if let Some(prek_skip) = prek_skip {
                command.env("PREK_SKIP", prek_skip);
            }
            outputs.push(command.output()?);
        }
        // Assert
        assert!(outputs.iter().all(|output| output.status.success()));
        assert_eq!(fixture.calls()?.len(), 2);
        let ledger: serde_json::Value = serde_json::from_slice(&fs::read(
            fixture
                .directory
                .path()
                .join("target/agentty-validation.json"),
        )?)?;
        assert!(ledger.get("clippy").is_some());
        assert!(ledger.get("test-focused").is_some());
    }

    Ok(())
}

#[test]
fn cargo_parent_context_does_not_leak_into_cataloged_checks() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fs::write(
        fixture.directory.path().join("prek"),
        r#"#!/bin/sh
if [ "$1" = --version ]; then
    echo prek-version
else
    printf '%s\n' "${CARGO_MANIFEST_DIR-unset}" "${CARGO_MANIFEST_PATH-unset}" \
        "${CARGO_MANIFEST_LINKS-unset}" "${CARGO_PKG_NAME-unset}" \
        "${CARGO_PKG_VERSION_MAJOR-unset}" "$AGENTTY_KEEP_ENV" "$CARGO_HOME" >> environment
    printf '%s|%s|%s\n' "$*" "$AGENTTY_TEST_PACKAGES" "$AGENTTY_TEST_FILTER" >> calls
fi
"#,
    )?;

    // Act
    let output = fixture
        .command(&[], 0)
        .env("CARGO_MANIFEST_DIR", "/cargo-parent/ag-xtask")
        .env("CARGO_MANIFEST_PATH", "/cargo-parent/ag-xtask/Cargo.toml")
        .env("CARGO_MANIFEST_LINKS", "parent-links")
        .env("CARGO_PKG_NAME", "ag-xtask")
        .env("CARGO_PKG_VERSION_MAJOR", "0")
        .env("AGENTTY_KEEP_ENV", "preserved")
        .env("CARGO_HOME", "unchanged-cache")
        .output()?;

    // Assert
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.directory.path().join("environment"))?,
        "unset\nunset\nunset\nunset\nunset\npreserved\nunchanged-cache\n".repeat(2)
    );
    assert_eq!(fixture.calls()?.len(), 2);

    Ok(())
}

#[test]
fn custom_targets_and_linked_outputs_never_invalidate_source_fingerprints()
-> Result<(), Box<dyn Error>> {
    // Arrange
    for target in ["build-cache", "build-cache[1]"] {
        let fixture = Fixture::new()?;
        let path = fixture.directory.path();
        fs::create_dir(path.join(target))?;
        fs::write(path.join("target-name"), target)?;
        std::os::unix::fs::symlink(target, path.join("target-alias"))?;
        std::os::unix::fs::symlink(format!("{target}/generated"), path.join("linked-output"))?;
        fs::write(
            path.join("git"),
            r#"#!/bin/sh
excluded=false
others=false
read -r expected < target-name
for argument in "$@"; do
    if [ "$argument" = ":(top,literal,exclude)$expected" ]; then excluded=true; fi
    case "$argument" in
        --others) others=true ;;
    esac
done
case "$1" in
    ls-files)
        printf 'source.rs\0'
        if "$others"; then
            printf 'target-alias\0linked-output\0'
            if ! "$excluded"; then find "$CARGO_TARGET_DIR" -type f -print0; fi
        fi
        ;;
    status)
        printf ' M source.rs\0?? target-alias\0?? linked-output\0'
        if ! "$excluded"; then
            find "$CARGO_TARGET_DIR" -type f -exec printf '?? %s\0' {} \;
        fi
        ;;
    *) echo git-state ;;
esac
"#,
        )?;
        fs::write(
            path.join("prek"),
            r#"#!/bin/sh
if [ "$1" = --version ]; then
    echo prek-version
else
    echo "$*" >> calls
    echo generated >> "$CARGO_TARGET_DIR/generated"
fi
"#,
        )?;
        // Act
        let first = fixture.run_in_target(&[], 0, target)?;
        let reused = fixture.run_in_target(&["--reuse"], 0, target)?;
        let unchanged = fixture.calls()?;
        fs::write(path.join(target).join("generated"), "new build output")?;
        let output_changed = fixture.run_in_target(&["--reuse"], 0, target)?;
        let output_calls = fixture.calls()?;
        fs::write(path.join("source.rs"), "new source")?;
        let source_changed = fixture.run_in_target(&["--reuse"], 0, target)?;
        // Assert
        for output in [first, reused, output_changed, source_changed] {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(unchanged.len(), 2);
        assert_eq!(output_calls.len(), 2);
        assert_eq!(fixture.calls()?.len(), 4);
        assert!(path.join(target).join("agentty-validation.json").exists());
        // Act / Assert: selecting the same directory through a link also
        // excludes its real path.
        let alias_first = fixture.run_in_target(&[], 0, "target-alias")?;
        let alias_reused = fixture.run_in_target(&["--reuse"], 0, "target-alias")?;
        for output in [alias_first, alias_reused] {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        assert_eq!(fixture.calls()?.len(), 6);
    }

    Ok(())
}

#[test]
fn targets_cannot_hide_the_workspace_or_tracked_sources() -> Result<(), Box<dyn Error>> {
    // Arrange
    for (target, diagnostic) in [(".", "workspace root"), ("inputs", "tracked source files")] {
        let fixture = Fixture::new()?;
        let path = fixture.directory.path();
        fs::create_dir(path.join("inputs"))?;
        fs::write(path.join("inputs/source.rs"), "tracked source")?;
        fs::write(
            path.join("git"),
            "#!/bin/sh\nprintf 'inputs/source.rs\\0'\n",
        )?;
        // Act
        let output = fixture.run_in_target(&[], 0, target)?;
        // Assert
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(diagnostic),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(!path.join("calls").exists());
    }

    Ok(())
}

#[test]
fn coordinated_clippy_keeps_its_scope_through_the_cataloged_command() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let fixture = Fixture::new()?;
    let path = fixture.directory.path();
    let catalog: Value = serde_yaml_ng::from_str(include_str!("../../../.pre-commit-config.yaml"))?;
    let hook = catalog["repos"]
        .as_sequence()
        .expect("repository list")
        .iter()
        .flat_map(|repository| repository["hooks"].as_sequence().expect("hook list"))
        .find(|hook| hook["id"].as_str() == Some("clippy"))
        .expect("Clippy hook");
    let entry = hook["entry"].as_str().expect("Clippy command");
    fs::write(path.join("clippy-hook"), format!("{entry} \"$@\"\n"))?;
    fs::write(
        path.join("prek"),
        r#"#!/bin/sh
if [ "$1" = --version ]; then echo prek-version; else
    exec sh clippy-hook Cargo.toml crates/other/src/lib.rs
fi
"#,
    )?;
    fs::write(
        path.join("cargo"),
        r#"#!/bin/sh
case "$1" in
    metadata) cat metadata.json ;;
    run)
        while [ "$1" != -- ]; do shift; done
        shift
        exec "$AGENTTY_XTASK" "$@"
        ;;
    clippy) printf '%s\n' "$*" >> lint-calls ;;
    *) echo nextest-version ;;
esac
"#,
    )?;

    // Act
    let output = fixture
        .command(&["--hook", "clippy"], 0)
        .env("AGENTTY_XTASK", env!("CARGO_BIN_EXE_ag-xtask"))
        .output()?;

    // Assert
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read_to_string(path.join("lint-calls"))?,
        "clippy -q --locked --all-targets --all-features -p app -p core -- -D warnings\n"
    );

    Ok(())
}

#[test]
fn cli_selects_dependents_and_reuses_only_unchanged_successes() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let first = fixture.run(&[], 0)?;
    let reused = fixture.run(&["--reuse"], 0)?;
    let first_calls = fixture.calls()?;
    fs::write(
        fixture.directory.path().join("source.rs"),
        "changed version",
    )?;
    let invalidated = fixture.run(&["--reuse"], 0)?;
    // Assert
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(reused.status.success());
    assert!(invalidated.status.success());
    assert_eq!(first_calls.len(), 2);
    assert_eq!(fixture.calls()?.len(), 4);
    assert_eq!(
        first_calls[0],
        "run clippy --all-files --hook-stage pre-commit|app core|package(=app) or package(=core)"
    );
    assert!(first_calls[1].contains("--hook-stage manual"));

    Ok(())
}

#[test]
fn failures_invalidate_success_and_coverage_always_executes() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    // Act
    let success = fixture.run(&["--hook", "clippy"], 0)?;
    let failure = fixture.run(&["--hook", "clippy"], 42)?;
    let ledger = fs::read_to_string(
        fixture
            .directory
            .path()
            .join("target/agentty-validation.json"),
    )?;
    let coverage = fixture.run(&["--hook", "coverage", "--reuse"], 0)?;
    let repeated = fixture.run(&["--hook", "coverage", "--reuse"], 0)?;
    // Assert
    assert!(success.status.success());
    assert!(!failure.status.success());
    assert_eq!(ledger, "{}");
    assert!(coverage.status.success());
    assert!(repeated.status.success());
    assert_eq!(fixture.calls()?.len(), 4);

    Ok(())
}

#[test]
fn malformed_inputs_and_invalid_selections_fail_without_running_checks()
-> Result<(), Box<dyn Error>> {
    // Arrange / Act / Assert
    for arguments in [
        vec!["--packages", "missing"],
        vec!["--hook", "missing"],
        vec!["--hook", "validate"],
    ] {
        let fixture = Fixture::new()?;
        assert!(!fixture.run(&arguments, 0)?.status.success());
        assert!(!fixture.directory.path().join("calls").exists());
    }
    for (file, content) in [
        ("metadata.json", "invalid"),
        (".pre-commit-config.yaml", "invalid"),
        ("target/agentty-validation.json", "invalid"),
    ] {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.directory.path().join("target"))?;
        fs::write(fixture.directory.path().join(file), content)?;
        assert!(!fixture.run(&[], 0)?.status.success());
        assert!(!fixture.directory.path().join("calls").exists());
    }

    Ok(())
}

#[test]
fn linked_directory_inputs_invalidate_reuse_without_following_cycles_forever()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let path = fixture.directory.path();
    fs::create_dir(path.join("inputs"))?;
    fs::write(path.join("inputs/data"), "first")?;
    std::os::unix::fs::symlink("inputs", path.join("linked-inputs"))?;
    std::os::unix::fs::symlink(".", path.join("inputs/cycle"))?;
    std::os::unix::fs::symlink("missing", path.join("inputs/dangling"))?;
    // Act
    let first = fixture.run(&["--hook", "clippy"], 0)?;
    let reuse = fixture.run(&["--hook", "clippy", "--reuse"], 0)?;
    let unchanged = fixture.calls()?;
    fs::write(path.join("inputs/data"), "changed")?;
    let changed = fixture.run(&["--hook", "clippy", "--reuse"], 0)?;
    // Assert
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(reuse.status.success());
    assert!(changed.status.success());
    assert_eq!(unchanged.len(), 1);
    assert_eq!(fixture.calls()?.len(), 2);

    Ok(())
}

#[test]
fn a_hook_executable_disappearing_after_input_collection_cannot_record_success()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let executable = fixture.directory.path().join("prek");
    fs::write(
        &executable,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo prek-version; rm \"$0\"; fi\n",
    )?;
    // Act
    let output = fixture.run(&["--hook", "clippy"], 0)?;
    // Assert
    assert!(!output.status.success());
    assert!(!fixture.directory.path().join("calls").exists());
    assert_eq!(
        fs::read_to_string(
            fixture
                .directory
                .path()
                .join("target/agentty-validation.json")
        )?,
        "{}"
    );

    Ok(())
}
