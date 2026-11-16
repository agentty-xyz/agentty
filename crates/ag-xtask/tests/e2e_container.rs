//! Process-level contracts for the cataloged E2E container command.

#![cfg(unix)]

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

#[path = "support/hook_catalog.rs"]
mod hook_catalog;

/// Workspace with stub `id` and `podman` executables and an isolated cache.
struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path();
        for (name, script) in [
            (
                "id",
                "if [ \"$1\" = -u ]; then echo \"${ID_USER:-501}\"; else echo 20; fi; exit \
                 \"${ID_EXIT:-0}\"",
            ),
            // `RUN_BLOCKS` keeps a labeled container listed by `ps` until the
            // test creates `release`; `wait` blocks while it is listed.
            (
                "podman",
                r#"case "$1" in
    ps) if [ -e running ]; then cat running; fi ;;
    wait)
        echo "$*" >> calls
        echo waiting-for-orphan
        while [ -e running ]; do sleep 0.05; done
        ;;
    run)
        if [ -e running ]; then echo overlap >> calls; fi
        echo "$*" >> calls
        if [ -n "${RUN_BLOCKS:-}" ]; then
            echo orphan > running
            echo container-started
            while [ ! -e release ]; do sleep 0.05; done
            rm running
        fi
        exit "${RUN_EXIT:-0}"
        ;;
    *) echo "$*" >> calls ;;
esac"#,
            ),
        ] {
            let executable = path.join(name);
            fs::write(&executable, format!("#!/bin/sh\n{script}\n"))?;
            fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
        }
        fs::create_dir_all(path.join("docs/site/content/features"))?;
        fs::create_dir_all(path.join("docs/site/static/features"))?;
        fs::write(
            path.join("docs/site/content/features/demo.md"),
            "+++\n[extra]\ngif = \"demo.gif\"\n+++\n",
        )?;
        fs::write(path.join("docs/site/static/features/demo.gif"), "GIF")?;

        Ok(Self { directory })
    }

    fn command(&self) -> Command {
        let path = self.directory.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ag-xtask"));
        command
            .arg("e2e-container")
            .current_dir(path)
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", path.display()))
            .env("XDG_CACHE_HOME", path.join("cache"));
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }

        command
    }

    fn cache(&self) -> PathBuf {
        self.directory.path().join("cache/agentty-e2e")
    }

    fn calls(&self) -> Result<Vec<String>, Box<dyn Error>> {
        Ok(fs::read_to_string(self.directory.path().join("calls"))?
            .lines()
            .map(str::to_string)
            .collect())
    }
}

/// Returns the logged failure, which `ag-xtask` writes to standard output.
fn logged(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn catalog_hook_runs_the_container_command() -> Result<(), Box<dyn Error>> {
    // Arrange, Act
    let entry = hook_catalog::entry("e2e-container")?;

    // Assert
    assert!(entry.ends_with("-p ag-xtask -- e2e-container"), "{entry}");

    Ok(())
}

#[test]
fn pinned_runs_pull_the_native_variant_and_mount_the_workspace() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let workspace = fixture.directory.path().canonicalize()?;

    // Act
    let output = fixture
        .command()
        .env(
            "AGENTTY_TEST_FILTER",
            "test(=navigation::test_help_overlay)",
        )
        .output()?;

    // Assert
    assert!(output.status.success(), "{}", logged(&output));
    let calls = fixture.calls()?;
    assert_eq!(calls.len(), 2);
    let platform = calls[0]
        .strip_prefix("pull --platform ")
        .and_then(|rest| rest.split_once(' '))
        .map(|(platform, image)| {
            assert!(image.contains("@sha256:"), "{image}");
            platform
        });
    assert!(matches!(platform, Some("linux/amd64" | "linux/arm64")));
    let run = &calls[1];
    assert!(
        run.starts_with("run --rm --pull=never --label agentty.e2e-container=1 --platform linux/")
    );
    assert!(run.contains(" --user 501:20 "));
    assert!(run.contains(&format!(
        "type=bind,source={},target=/workspace",
        workspace.display()
    )));
    assert!(run.contains(" --env TESTTY_GIF_MODE=check "));
    assert!(run.ends_with(" -E test(=navigation::test_help_overlay)"));
    let target = run
        .split_once(" --env CARGO_TARGET_DIR=/cache/")
        .and_then(|(_, rest)| rest.split_once(' '))
        .map(|(target, _)| target)
        .ok_or("missing checkout build directory")?;
    assert!(target.starts_with("target-"), "{target}");
    for name in ["home", "cargo", "prek", target] {
        assert!(fixture.cache().join(name).is_dir(), "{name}");
    }

    Ok(())
}

#[test]
fn marketing_assets_run_the_showcase_generators() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let output = fixture
        .command()
        .env("AGENTTY_E2E_IMAGE", "sha256:candidate")
        .env("AGENTTY_MARKETING_ASSETS", "sessions tab_tour")
        .output()?;

    // Assert
    assert!(output.status.success(), "{}", logged(&output));
    let calls = fixture.calls()?;
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].ends_with(
            " sha256:candidate cargo test --locked -p agentty --test showcase -- sessions tab_tour"
        ),
        "{}",
        calls[0]
    );

    Ok(())
}

#[test]
fn invalid_asset_selections_fail_before_any_container_runs() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let unknown = fixture
        .command()
        .env("AGENTTY_MARKETING_ASSETS", "hero")
        .output()?;
    let filtered = fixture
        .command()
        .env("AGENTTY_MARKETING_ASSETS", "demo")
        .env("AGENTTY_TEST_FILTER", "test(help)")
        .output()?;

    // Assert
    assert!(!unknown.status.success());
    assert!(logged(&unknown).contains("AGENTTY_MARKETING_ASSETS names `hero`"));
    assert!(!filtered.status.success());
    assert!(logged(&filtered).contains("cannot be combined with AGENTTY_TEST_FILTER"));
    assert!(!fixture.directory.path().join("calls").exists());

    Ok(())
}

#[test]
fn a_second_run_waits_for_the_lock_holder() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fs::create_dir_all(fixture.cache())?;
    let holder = File::create(fixture.cache().join("container.lock"))?;
    holder.lock()?;

    // Act
    let mut child = fixture
        .command()
        .env("AGENTTY_E2E_IMAGE", "sha256:candidate")
        .env("TESTTY_GIF_MODE", "generate")
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stderr = BufReader::new(child.stderr.take().ok_or("missing stderr")?);
    let mut waiting = String::new();
    stderr.read_line(&mut waiting)?;
    let ran_while_locked = fixture.directory.path().join("calls").exists();
    drop(holder);
    let status = child.wait()?;

    // Assert
    assert!(waiting.starts_with("Waiting for another Agentty E2E container run"));
    assert!(!ran_while_locked);
    assert!(status.success());
    let calls = fixture.calls()?;
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains(" --env TESTTY_GIF_MODE=generate sha256:candidate cargo "));

    Ok(())
}

/// Lets a blocked stub container exit, including when an assertion fails.
struct Release(PathBuf);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "");
    }
}

#[test]
fn a_killed_run_keeps_later_runs_waiting_for_its_container() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    let release = Release(fixture.directory.path().join("release"));
    let mut first = fixture
        .command()
        .env("AGENTTY_E2E_IMAGE", "sha256:first")
        .env("RUN_BLOCKS", "1")
        .stdout(Stdio::piped())
        // The orphaned stub outlives this child; keep it off the test's
        // own output streams.
        .stderr(Stdio::null())
        .spawn()?;
    let mut started = String::new();
    BufReader::new(first.stdout.take().ok_or("missing stdout")?).read_line(&mut started)?;

    // Act
    first.kill()?;
    first.wait()?;
    let mut second = fixture
        .command()
        .env("AGENTTY_E2E_IMAGE", "sha256:second")
        .stdout(Stdio::piped())
        .spawn()?;
    let mut waiting = String::new();
    BufReader::new(second.stdout.take().ok_or("missing stdout")?).read_line(&mut waiting)?;
    let started_while_orphaned = fixture
        .calls()?
        .iter()
        .any(|call| call.contains("sha256:second"));
    drop(release);
    let status = second.wait()?;

    // Assert
    assert_eq!(started.trim(), "container-started");
    assert_eq!(waiting.trim(), "waiting-for-orphan");
    assert!(!started_while_orphaned);
    assert!(status.success());
    let calls = fixture.calls()?;
    assert!(calls.iter().any(|call| call == "wait --ignore orphan"));
    assert!(!calls.iter().any(|call| call == "overlap"));
    assert!(
        calls
            .last()
            .is_some_and(|call| call.contains("sha256:second"))
    );

    Ok(())
}

#[test]
fn failures_propagate_to_the_exit_status() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let identity = fixture.command().env("ID_EXIT", "1").output()?;
    let container = fixture.command().env("RUN_EXIT", "3").output()?;
    fs::write(
        fixture
            .directory
            .path()
            .join("docs/site/static/features/demo.gif"),
        "",
    )?;
    let recording = fixture.command().output()?;

    // Assert
    assert!(!identity.status.success());
    assert!(!container.status.success());
    assert!(logged(&container).contains("podman run failed"));
    assert!(!recording.status.success());
    assert!(logged(&recording).contains("demo.gif"));

    Ok(())
}
