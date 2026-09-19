//! Exercise launcher failures in isolated processes, never in the test runner.

use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::time::Duration;

use serde_json::json;
#[cfg(target_os = "linux")]
use tokio::io::AsyncBufReadExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::coverage::launcher;

#[tokio::test]
async fn malformed_launcher_protocol_reports_failure_and_exits() {
    // Arrange
    for input in [
        Vec::new(),
        b"not JSON\n".to_vec(),
        vec![b'x'; 2 * 1024 * 1024],
    ] {
        // Act / Assert
        rejected(&[], &input).await;
    }
}

#[tokio::test]
async fn missing_enforcement_or_executable_reports_failure_and_exits() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("root");
    let mut configuration = json!({
        "arguments": [], "directory": root, "environment": {},
        "executable": root.join("missing-executable"), "external_reads": [],
        "git_metadata": [], "host_information": true, "launcher": launcher(),
        "linux_bubblewrap": null, "workspace": root, "workspace_writes": [],
    });

    // Act / Assert
    #[cfg(target_os = "linux")]
    {
        rejected(&["--namespace-init"], &[]).await;
        let mut encoded = serde_json::to_vec(&configuration).expect("configuration");
        encoded.push(b'\n');
        rejected(&[], &encoded).await;
        configuration["linux_bubblewrap"] = json!(root.join("missing-bwrap"));
        configuration["git_metadata"] = json!([root.join("missing-metadata")]);
        let mut encoded = serde_json::to_vec(&configuration).expect("configuration");
        encoded.push(b'\n');
        rejected(&[], &encoded).await;
    }
    #[cfg(target_os = "macos")]
    {
        configuration["host_information"] = json!(false);
        let mut encoded = serde_json::to_vec(&configuration).expect("configuration");
        encoded.push(b'\n');
        rejected(&["--seatbelt-child"], &encoded).await;
    }
}

async fn rejected(arguments: &[&str], input: &[u8]) {
    let (parent, child) = std::os::unix::net::UnixStream::pair().expect("channel");
    parent.set_nonblocking(true).expect("nonblocking");
    let mut parent = tokio::net::UnixStream::from_std(parent).expect("async channel");
    let child: OwnedFd = child.into();
    let mut child = tokio::process::Command::new(launcher())
        .args(arguments)
        .stdin(Stdio::from(child))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("launcher");
    tokio::time::timeout(Duration::from_secs(10), async {
        parent.write_all(input).await.expect("configuration");
        parent.shutdown().await.expect("configuration EOF");
        let mut notice = Vec::new();
        parent
            .read_to_end(&mut notice)
            .await
            .expect("failure notice");
        let expected = if arguments == ["--seatbelt-child"] {
            &b""[..]
        } else {
            &b"\"Failed\"\n"[..]
        };
        assert_eq!(notice, expected);
        assert!(!child.wait().await.expect("exit").success());
    })
    .await
    .expect("bounded launcher failure");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn private_channel_cancellation_requires_host_process_group_cleanup() {
    // Arrange
    let workspace = super::fixture::Workspace::new();
    let root = workspace.path();
    let configuration = json!({
        "arguments": ["--noprofile", "--norc", "-c", "printf started > output/started; exec /bin/sleep 30"],
        "directory": root, "environment": {}, "executable": "/bin/bash",
        "external_reads": ["/bin", "/usr/lib", "/System/Library", launcher()],
        "git_metadata": [], "host_information": true, "launcher": workspace.launcher(),
        "linux_bubblewrap": null, "workspace": root, "workspace_writes": ["output"],
    });
    let (parent, child) = std::os::unix::net::UnixStream::pair().expect("channel");
    parent.set_nonblocking(true).expect("nonblocking");
    let mut parent = tokio::net::UnixStream::from_std(parent).expect("async channel");
    let child: OwnedFd = child.into();
    let mut child = tokio::process::Command::new(workspace.launcher())
        .stdin(Stdio::from(child))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .expect("launcher");
    let group = ProcessGroup(
        rustix::process::Pid::from_raw(i32::try_from(child.id().expect("pid")).expect("pid"))
            .expect("pid"),
    );
    let mut encoded = serde_json::to_vec(&configuration).expect("configuration");
    encoded.push(b'\n');

    // Act
    let notice = tokio::time::timeout(Duration::from_secs(10), async {
        parent.write_all(&encoded).await.expect("configuration");
        while !root.join("output/started").exists() {
            assert!(
                child.try_wait().expect("launcher status").is_none(),
                "launcher exited before starting command"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        parent.write_all(b"x").await.expect("cancel");
        let mut notice = Vec::new();
        parent.read_to_end(&mut notice).await.expect("notice");
        notice
    })
    .await
    .expect("bounded cancellation");
    let status = child.wait().await.expect("launcher exit");
    drop(group);

    // Assert
    assert_eq!(notice, b"\"Failed\"\n");
    assert!(
        !status.success(),
        "channel cancellation alone cannot confirm macOS cleanup"
    );
}

#[cfg(target_os = "macos")]
struct ProcessGroup(rustix::process::Pid);

#[cfg(target_os = "macos")]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}

/// A launched Linux namespace driven at the launcher wire. The wire retains
/// write mounts so these tests can observe namespace semantics and persist
/// inner launcher coverage even though the production policy rejects Linux
/// write grants.
#[cfg(target_os = "linux")]
struct Namespace {
    child: tokio::process::Child,
    notices: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    sender: tokio::net::unix::OwnedWriteHalf,
    stdout: tokio::process::ChildStdout,
}

#[cfg(target_os = "linux")]
impl Namespace {
    async fn start(workspace: &super::fixture::Workspace, command: &str) -> Self {
        let root = workspace.path();
        let external_reads: Vec<std::path::PathBuf> =
            super::fixture::runtime_reads(&[]).into_iter().collect();
        let configuration = json!({
            "arguments": ["-c", command],
            "directory": root, "environment": {}, "executable": "/bin/bash",
            "external_reads": external_reads,
            "git_metadata": [root.join(".git")], "host_information": true,
            "launcher": workspace.launcher(), "linux_bubblewrap": "/usr/bin/bwrap",
            "workspace": root, "workspace_writes": ["output"],
        });
        let (parent, child) = std::os::unix::net::UnixStream::pair().expect("channel");
        parent.set_nonblocking(true).expect("nonblocking");
        let parent = tokio::net::UnixStream::from_std(parent).expect("async channel");
        let child: OwnedFd = child.into();
        let mut child = tokio::process::Command::new(workspace.launcher())
            .env_clear()
            .current_dir("/")
            .stdin(Stdio::from(child))
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("launcher");
        let stdout = child.stdout.take().expect("launcher output");
        let (receiver, mut sender) = parent.into_split();
        let mut encoded = serde_json::to_vec(&configuration).expect("configuration");
        encoded.push(b'\n');
        sender.write_all(&encoded).await.expect("configuration");
        let mut namespace = Self {
            child,
            notices: tokio::io::BufReader::new(receiver),
            sender,
            stdout,
        };
        assert_eq!(namespace.notice().await, "\"Configure\"");
        namespace
            .sender
            .write_all(&encoded)
            .await
            .expect("namespace configuration");

        namespace
    }

    async fn notice(&mut self) -> String {
        let mut line = String::new();
        self.notices.read_line(&mut line).await.expect("notice");

        line.trim_end().to_owned()
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn namespace_completion_waits_for_surviving_descendants_and_persists_inner_profiles() {
    // Arrange
    let workspace = super::fixture::Workspace::new();
    let mut namespace = Namespace::start(
        &workspace,
        "(/bin/sleep 0.3; printf child > output/child) & printf main; exit 5",
    )
    .await;

    // Act / Assert
    tokio::time::timeout(Duration::from_secs(10), async {
        assert_eq!(
            namespace.notice().await,
            "{\"MainExit\":{\"code\":5,\"signal\":null}}"
        );
        assert_eq!(namespace.notice().await, "\"Finished\"");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("output/child"))
                .expect("descendant completed before Finished"),
            "child"
        );
        namespace.sender.write_all(b"x").await.expect("acknowledge");
        let mut output = Vec::new();
        namespace
            .stdout
            .read_to_end(&mut output)
            .await
            .expect("shell output");
        assert_eq!(output, b"main");
        assert!(
            namespace.child.wait().await.expect("exit").success(),
            "acknowledged namespace completion"
        );
    })
    .await
    .expect("bounded namespace completion");
    workspace.assert_phase_profiles(&["outer-", "inner-"]);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn namespace_cancellation_instruction_settles_the_namespace() {
    // Arrange
    let workspace = super::fixture::Workspace::new();
    let mut namespace = Namespace::start(&workspace, "printf started; exec /bin/sleep 30").await;

    // Act / Assert
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut started = [0; 7];
        namespace
            .stdout
            .read_exact(&mut started)
            .await
            .expect("command start");
        assert_eq!(&started, b"started");
        namespace.sender.write_all(b"x").await.expect("cancel");
        let mut remaining = Vec::new();
        namespace
            .notices
            .read_to_end(&mut remaining)
            .await
            .expect("channel close");
        assert_eq!(remaining, b"", "cancellation ends the namespace silently");
        assert!(
            namespace.child.wait().await.expect("exit").success(),
            "kernel reaps namespace descendants on cancellation"
        );
    })
    .await
    .expect("bounded namespace cancellation");
}
