//! Own the entire native fixture compiler group, including cancellation
//! cleanup.

use std::future::pending;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;
use std::{fs, io};

use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

struct CompilerGroup {
    child: Option<Child>,
    group: Pid,
}

impl CompilerGroup {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        let child = command
            .process_group(0)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let group = Pid::from_raw(i32::try_from(child.id().expect("unreaped child")).expect("pid"))
            .expect("positive pid");

        Ok(Self {
            child: Some(child),
            group,
        })
    }

    async fn wait_for_exit(&self) -> io::Result<()> {
        // Do not reap the leader until after signalling the group: keeping it
        // waitable prevents its PID from being reused as an unrelated group.
        while process::waitid(
            WaitId::Pid(self.group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?
        .is_none()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        process::kill_process_group(self.group, Signal::KILL)?;

        Ok(())
    }

    async fn finish(&mut self) -> io::Result<std::process::ExitStatus> {
        process::kill_process_group(self.group, Signal::KILL)?;
        let status = self.child.as_mut().expect("owned child").wait().await?;
        self.child = None;

        Ok(status)
    }
}

impl Drop for CompilerGroup {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = process::kill_process_group(self.group, Signal::KILL);
            // Retain reaping independently of a cancelled compilation future.
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

pub(super) async fn output(command: &mut Command, deadline: Duration) -> io::Result<Output> {
    let mut compiler = CompilerGroup::spawn(command)?;
    let child = compiler.child.as_mut().expect("owned child");
    let mut stdout = child.stdout.take().expect("compiler stdout");
    let mut stderr = child.stderr.take().expect("compiler stderr");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(deadline, async {
        tokio::try_join!(
            compiler.wait_for_exit(),
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
        )
    })
    .await;
    let status = compiler.finish().await?;
    result.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "compiler deadline"))??;

    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

#[tokio::test]
async fn compiler_groups_are_cleaned_up_after_timeout_cancellation_and_driver_exit() {
    // Arrange: re-executed copies form a real driver/child/grandchild tree.
    if let Some(root) = std::env::var_os("AG_HARNESS_COMPILER_ROOT") {
        run_compiler_tree(Path::new(&root)).await;

        return;
    }
    for mode in ["timeout", "cancel", "exit"] {
        let root = tempfile::tempdir().expect("owned compiler fixture");
        let mut command = compiler_tree(root.path(), 0, mode);
        let mut running = Box::pin(output(&mut command, Duration::from_secs(10)));
        tokio::select! {
            result = &mut running => panic!("compiler exited before readiness: {result:?}"),
            () = wait_for_tree(root.path()) => {}
        }
        let pids: Vec<_> = (0..3)
            .map(|stage| {
                let pid = fs::read_to_string(root.path().join(format!("{stage}.pid")))
                    .expect("owned pid")
                    .parse::<i32>()
                    .expect("pid number");

                Pid::from_raw(pid).expect("positive pid")
            })
            .collect();
        for pid in &pids {
            assert_eq!(process::getpgid(Some(*pid)).expect("live control"), pids[0]);
        }

        // Act
        match mode {
            "timeout" => assert_eq!(
                running.await.expect_err("timeout").kind(),
                io::ErrorKind::TimedOut
            ),
            "cancel" => drop(running),
            _ => {
                fs::write(root.path().join("exit"), "release driver").expect("driver gate");
                let result = running.await.expect("driver output");
                assert!(result.status.success());
                assert!(String::from_utf8_lossy(&result.stdout).contains("driver output"));
                assert!(String::from_utf8_lossy(&result.stderr).contains("driver diagnostic"));
            }
        }

        // Assert: both descendants and the direct driver are reaped.
        tokio::time::timeout(Duration::from_secs(5), async {
            while pids
                .iter()
                .any(|pid| PathBuf::from(format!("/proc/{pid}")).exists())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("compiler group reaped");
    }
}

fn compiler_tree(root: &Path, stage: u8, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .env("AG_HARNESS_COMPILER_ROOT", root)
        .env("AG_HARNESS_COMPILER_STAGE", stage.to_string())
        .env("AG_HARNESS_COMPILER_MODE", mode)
        .args([
            "--exact",
            "execution::linux::launch::tests::compiler::compiler_groups_are_cleaned_up_after_timeout_cancellation_and_driver_exit",
            "--nocapture",
        ]);

    command
}

async fn run_compiler_tree(root: &Path) {
    let stage: u8 = std::env::var("AG_HARNESS_COMPILER_STAGE")
        .expect("compiler stage")
        .parse()
        .expect("stage number");
    let mode = std::env::var("AG_HARNESS_COMPILER_MODE").expect("compiler mode");
    // The outer CompilerGroup owns these children from spawn. Let descendants
    // survive driver exit so the regression requires group-wide cleanup.
    let _child = (stage < 2).then(|| {
        compiler_tree(root, stage + 1, &mode)
            .spawn()
            .expect("compiler descendant")
    });
    fs::write(
        root.join(format!("{stage}.pending")),
        std::process::id().to_string(),
    )
    .expect("compiler readiness");
    fs::rename(
        root.join(format!("{stage}.pending")),
        root.join(format!("{stage}.pid")),
    )
    .expect("publish complete pid");
    if stage == 0 && mode == "exit" {
        while !root.join("exit").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        writeln!(io::stdout(), "driver output").expect("driver stdout");
        writeln!(io::stderr(), "driver diagnostic").expect("driver stderr");
    } else {
        pending::<()>().await;
    }
}

async fn wait_for_tree(root: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while (0..3).any(|stage| !root.join(format!("{stage}.pid")).exists()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("compiler tree readiness");
}
