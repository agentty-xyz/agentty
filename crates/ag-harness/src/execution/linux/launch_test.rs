use std::fs;
use std::future::{Future, poll_fn};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::task::Poll;
use std::time::Duration;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::{Configuration, Launch, close_descriptors, syscall_result};
use crate::execution::contract::{Command as IsolatedCommand, Grants, Policy};

#[path = "compiler_test.rs"]
mod compiler;

struct Fixture {
    root: TempDir,
    configuration: Configuration,
    entrypoint: PathBuf,
    probe: PathBuf,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_mode("isolated").await
    }

    async fn with_mode(mode: &str) -> Self {
        let root = tempfile::tempdir().expect("fixture");
        let base = root.path().canonicalize().expect("canonical fixture");
        let workspace = base.join("workspace");
        let runtime = base.join("runtime");
        let scratch = base.join("scratch");
        for path in [&workspace, &runtime, &scratch, &workspace.join(".git")] {
            fs::create_dir(path).expect("fixture directory");
        }
        for path in [
            workspace.join("readonly"),
            workspace.join("writable"),
            workspace.join(".git/config"),
            base.join("secret"),
            runtime.join("resource"),
        ] {
            fs::write(path, "original").expect("fixture file");
        }
        let probe = runtime.join("probe");
        build_fixture("probe_test.c", "AG_HARNESS_LINUX_PROBE", &probe).await;
        let entrypoint = runtime.join("entrypoint");
        build_fixture("entrypoint.c", "AG_HARNESS_LINUX_ENTRYPOINT", &entrypoint).await;
        let policy = Policy::new(
            workspace.clone(),
            vec![workspace.join(".git")],
            Grants {
                host_information: true,
                environment: vec![("ALLOWED".into(), "literal value".into())],
                external_reads: vec![runtime.clone()],
                workspace_writes: vec!["writable".into()],
                ..Grants::default()
            },
        )
        .expect("policy");
        let command = IsolatedCommand::new(
            probe.clone(),
            vec![
                mode.into(),
                base.join("secret").into_os_string(),
                runtime.join("resource").into_os_string(),
            ],
            ".".into(),
        )
        .expect("command");
        let configuration = Configuration::new(command, policy, scratch).expect("configuration");

        Self {
            root,
            configuration,
            entrypoint,
            probe,
        }
    }
}

async fn build_fixture(source: &str, prebuilt: &str, destination: &Path) {
    if let Some(prebuilt) = std::env::var_os(prebuilt) {
        fs::copy(prebuilt, destination).expect("prebuilt native fixture");
    } else {
        let bubblewrap =
            std::env::var("AG_HARNESS_BWRAP").unwrap_or_else(|_| "/usr/bin/bwrap".into());
        let mut compiler = Command::new("musl-gcc");
        compiler
            .args(["-static", "-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg(format!("-DBWRAP_PATH=\"{bubblewrap}\""))
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/execution/linux")
                    .join(source),
            )
            .arg("-o")
            .arg(destination);
        let output = compiler::output(&mut compiler, Duration::from_secs(30))
            .await
            .expect("compiler");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[tokio::test]
async fn real_process_enforces_policy_with_host_and_isolated_positive_controls() {
    // Arrange
    let fixture = Fixture::new().await;
    let base = fixture.root.path();
    let secret = fs::File::open(base.join("secret")).expect("host secret");
    let inherited = rustix::io::fcntl_dupfd_cloexec(&secret, 42).expect("owned descriptor");
    rustix::io::fcntl_setfd(&inherited, rustix::io::FdFlags::empty()).expect("inheritable control");
    let output = Command::new(&fixture.probe)
        .kill_on_drop(true)
        .current_dir(fixture.configuration.policy().workspace())
        .arg("host")
        .arg(base.join("secret"))
        .arg(base.join("runtime/resource"))
        .arg(inherited.as_raw_fd().to_string())
        .output()
        .await
        .expect("host positive controls");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = fs::read(base.join("workspace/.git/config")).expect("Git content");

    // Act
    let mut launch = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
        .await
        .expect("spawn owned sandbox");
    let scratch = launch.scratch.path().to_path_buf();
    let status = tokio::time::timeout(Duration::from_secs(10), launch.child.wait())
        .await
        .expect("sandbox deadline")
        .expect("sandbox wait");
    let mut stderr = String::new();
    launch
        .child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .await
        .expect("read stderr");
    let mut stdout = String::new();
    launch
        .child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .await
        .expect("read stdout");
    launch
        .stop()
        .await
        .expect("idempotent stop after completion");
    drop(launch);

    // Assert
    assert!(status.success(), "{stderr}");
    assert_eq!(stdout, "isolation controls passed\n");
    assert_eq!(
        fs::read(base.join("workspace/.git/config")).expect("Git content"),
        before
    );
    assert!(
        fs::read_to_string(base.join("workspace/writable"))
            .expect("content")
            .starts_with("changed")
    );
    assert!(!scratch.exists());
}

#[tokio::test]
async fn launch_rejects_replaced_source_and_invalid_backend_without_starting() {
    // Arrange
    let fixture = Fixture::new().await;

    // Act / Assert
    assert!(
        Launch::start(
            &fixture.configuration,
            Path::new("missing"),
            &fixture.entrypoint
        )
        .await
        .is_err()
    );
    assert!(
        Launch::start(
            &fixture.configuration,
            fixture.root.path(),
            &fixture.entrypoint
        )
        .await
        .is_err()
    );
    fs::set_permissions(&fixture.probe, fs::Permissions::from_mode(0o4755))
        .expect("setuid fixture");
    assert!(
        Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
            .await
            .is_err()
    );
    fs::set_permissions(&fixture.probe, fs::Permissions::from_mode(0o755)).expect("reset fixture");
    fs::hard_link(
        fixture.root.path().join("workspace/writable"),
        fixture.root.path().join("alias"),
    )
    .expect("late hardlink");
    assert!(
        Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_dir(fixture.configuration.scratch())
            .expect("scratch")
            .count(),
        0
    );
}

#[tokio::test]
async fn spawn_failure_never_retries_payload_and_cleans_resources() {
    // Arrange
    let fixture = Fixture::new().await;
    let backend = fixture.root.path().join("invalid-backend");
    fs::write(&backend, "not an executable image").expect("invalid backend");
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).expect("executable mode");

    // Act
    let result = Launch::start(&fixture.configuration, &backend, &fixture.entrypoint).await;

    // Assert
    assert!(result.is_err());
    fs::copy(&fixture.probe, &backend).expect("valid ELF backend");
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o644))
        .expect("non-executable backend");
    assert!(
        Launch::start(&fixture.configuration, &backend, &fixture.entrypoint)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_dir(fixture.configuration.scratch())
            .expect("scratch")
            .count(),
        0
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("workspace/writable")).expect("content"),
        "original"
    );
}

#[tokio::test]
async fn stop_terminates_real_descendants_and_releases_scratch() {
    // Arrange
    let fixture = Fixture::with_mode("linger").await;
    let mut launch = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
        .await
        .expect("owned launch");
    let scratch = launch.scratch.path().to_path_buf();
    let mut output = BufReader::new(launch.child.stdout.take().expect("stdout"));
    let mut ready = String::new();
    tokio::time::timeout(Duration::from_secs(10), output.read_line(&mut ready))
        .await
        .expect("ready deadline")
        .expect("ready read");
    assert_eq!(ready, "ready\n");
    let mut descendants = Vec::new();
    collect_descendants(launch.child.id().expect("running pid"), &mut descendants);
    assert!(descendants.len() >= 2, "real namespace and forked process");

    // Act
    launch.stop().await.expect("stop and reap");
    drop(launch);
    tokio::time::timeout(Duration::from_secs(5), async {
        while descendants
            .iter()
            .any(|pid| Path::new(&format!("/proc/{pid}")).exists())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all descendants reaped by host init");

    // Assert
    assert!(!scratch.exists());
}

fn collect_descendants(parent: u32, descendants: &mut Vec<u32>) {
    let children = fs::read_to_string(format!("/proc/{parent}/task/{parent}/children"))
        .expect("owned child process tree");
    for child in children.split_whitespace() {
        let child = child.parse().expect("child pid");
        descendants.push(child);
        collect_descendants(child, descendants);
    }
}

#[test]
fn descriptor_setup_propagates_kernel_errors() {
    // Arrange / Act / Assert
    assert!(syscall_result(0).is_ok());
    assert!(syscall_result(-1).is_err());
}

#[tokio::test]
async fn descriptor_syscall_marks_inheritable_handles_without_closing_them_early() {
    // Arrange / Act / Assert
    if std::env::var_os("AG_HARNESS_DESCRIPTOR_CHILD").is_none() {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .kill_on_drop(true)
            .env("AG_HARNESS_DESCRIPTOR_CHILD", "1")
            .args(["--exact", "execution::linux::launch::tests::descriptor_syscall_marks_inheritable_handles_without_closing_them_early"])
            .output()
            .await
            .expect("owned descriptor probe");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        return;
    }
    // Arrange
    let descriptor = tempfile::tempfile().expect("owned descriptor");
    rustix::io::fcntl_setfd(&descriptor, rustix::io::FdFlags::empty())
        .expect("inheritable control");
    assert!(
        rustix::io::fcntl_getfd(&descriptor)
            .expect("descriptor flags")
            .is_empty()
    );

    // Act
    close_descriptors().expect("close-on-exec syscall");

    // Assert
    assert!(
        rustix::io::fcntl_getfd(&descriptor)
            .expect("still open")
            .contains(rustix::io::FdFlags::CLOEXEC)
    );
}

#[tokio::test]
async fn source_quiescence_ends_only_after_the_validated_mounts_are_pinned() {
    // Arrange
    let fixture = Fixture::with_mode("pinned").await;
    let gate = fixture.root.path().join("setup-gate");
    fs::write(&gate, "blocked").expect("setup gate");
    let mut starting = Box::pin(Launch::start(
        &fixture.configuration,
        &fixture.probe,
        &fixture.entrypoint,
    ));
    let entered = fixture.root.path().join("setup-entered");

    // Act
    tokio::select! {
        result = &mut starting => panic!("launch returned before setup: {}", result.is_ok()),
        () = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !entered.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.expect("wrapper reached gate");
        } => {}
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut starting)
            .await
            .is_err()
    );
    fs::remove_file(gate).expect("release setup");
    let mut launch = starting.await.expect("mount readiness");
    let workspace = fixture.configuration.policy().workspace();
    let original = fixture.root.path().join("original-workspace");
    fs::rename(workspace, &original).expect("release quiescence and replace source");
    fs::create_dir(workspace).expect("replacement workspace");
    fs::write(workspace.join("readonly"), "replacement").expect("replacement read");
    fs::write(workspace.join("writable"), "replacement").expect("replacement write");
    fs::write(launch.scratch.path().join("payload/release"), "continue").expect("release payload");
    let status = tokio::time::timeout(Duration::from_secs(5), launch.child.wait())
        .await
        .expect("payload deadline")
        .expect("payload status");

    // Assert
    assert!(status.success());
    assert!(
        fs::read_to_string(original.join("writable"))
            .expect("original content")
            .starts_with("changed")
    );
    assert_eq!(
        fs::read_to_string(workspace.join("writable")).expect("replacement content"),
        "replacement"
    );
}

#[tokio::test]
async fn failed_or_stalled_mount_setup_is_rejected_and_cleaned_up() {
    // Arrange
    let fixture = Fixture::new().await;

    // Act / Assert
    for (marker, expected) in [
        ("setup-fail", std::io::ErrorKind::Other),
        ("setup-gate", std::io::ErrorKind::TimedOut),
    ] {
        let path = fixture.root.path().join(marker);
        fs::write(&path, "stop").expect("setup marker");
        let error = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
            .await
            .err()
            .expect("reject incomplete setup");
        assert_eq!(error.kind(), expected);
        assert_eq!(
            fs::read_dir(fixture.configuration.scratch())
                .expect("scratch")
                .count(),
            0
        );
        fs::remove_file(path).expect("remove marker");
    }
}

#[tokio::test]
async fn hostile_cleanup_cannot_erase_readiness_before_the_host_observes_it() {
    // Arrange
    let fixture = Fixture::with_mode("cleanup").await;
    let gate = fixture.root.path().join("setup-gate");
    fs::write(&gate, "blocked").expect("hold setup until the first poll completes");
    let mut starting = Box::pin(Launch::start(
        &fixture.configuration,
        &fixture.probe,
        &fixture.entrypoint,
    ));
    let writable = fixture.configuration.policy().workspace().join("writable");

    // Act
    assert!(
        poll_fn(|context| Poll::Ready(starting.as_mut().poll(context)))
            .await
            .is_pending()
    );
    fs::remove_file(gate).expect("release trusted setup");
    // Keep the launch future unpolled until the real payload has attempted
    // removal. This deterministically exercises a delayed host observer.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !fs::read_to_string(&writable)
        .expect("payload progress")
        .starts_with("changed")
    {
        assert!(
            std::time::Instant::now() < deadline,
            "payload cleanup deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut launch = starting.await.expect("readiness survives hostile cleanup");
    let entrypoint = launch.scratch.path().join("control/entry");
    launch.stop().await.expect("stop payload");

    // Assert
    assert!(entrypoint.is_file());
    assert!(
        !launch
            .scratch
            .path()
            .join("payload/.ag-mounts-ready")
            .exists()
    );
    drop(launch);
    assert!(!entrypoint.exists());
}

#[tokio::test]
async fn active_scratch_contents_do_not_block_launches_sharing_a_configuration() {
    // Arrange
    let fixture = Fixture::with_mode("cleanup").await;
    let mut first = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
        .await
        .expect("first launch");
    let mut output = BufReader::new(first.child.stdout.take().expect("stdout"));
    let mut ready = String::new();
    tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut ready))
        .await
        .expect("first payload deadline")
        .expect("first payload ready");
    assert_eq!(ready, "ready\n");
    let first_scratch = first.scratch.path().to_path_buf();
    assert!(
        fs::symlink_metadata(first_scratch.join("payload/escape"))
            .expect("active launch created a symlink")
            .is_symlink()
    );

    // Act
    let mut second = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
        .await
        .expect("concurrent launch with shared configuration");
    let second_scratch = second.scratch.path().to_path_buf();
    let mut output = BufReader::new(second.child.stdout.take().expect("stdout"));
    ready.clear();
    tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut ready))
        .await
        .expect("second payload deadline")
        .expect("second payload ready");
    first.stop().await.expect("stop first payload");
    drop(first);

    // Assert
    assert_eq!(ready, "ready\n");
    assert_ne!(first_scratch, second_scratch);
    assert!(!first_scratch.exists());
    assert!(second_scratch.join("control/entry").is_file());
    assert!(
        second
            .child
            .try_wait()
            .expect("second still running")
            .is_none()
    );
    second.stop().await.expect("stop second payload");
    drop(second);
    assert!(!second_scratch.exists());
}

#[tokio::test]
async fn setup_failure_after_a_mount_marker_is_rejected_and_cleaned_up() {
    // Arrange
    let fixture = Fixture::new().await;
    let gate = fixture.root.path().join("setup-gate");
    fs::write(&gate, "blocked").expect("setup gate");
    fs::write(
        fixture.root.path().join("setup-late-fail"),
        "fail after marker",
    )
    .expect("late failure injection");
    let mut starting = Box::pin(Launch::start(
        &fixture.configuration,
        &fixture.probe,
        &fixture.entrypoint,
    ));

    // Act
    assert!(
        poll_fn(|context| Poll::Ready(starting.as_mut().poll(context)))
            .await
            .is_pending()
    );
    fs::remove_file(gate).expect("release setup");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !fs::read_dir(fixture.configuration.scratch())
        .expect("owned scratch")
        .any(|entry| {
            entry
                .expect("launch directory")
                .path()
                .join("payload/setup-reached")
                .is_dir()
        })
    {
        assert!(
            std::time::Instant::now() < deadline,
            "bubblewrap reached the late setup step"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let error = starting
        .await
        .err()
        .expect("reject failure after marker creation");

    // Assert
    assert!(
        error.to_string().contains("did not complete setup"),
        "{error}"
    );
    assert_eq!(
        fs::read_dir(fixture.configuration.scratch())
            .expect("scratch")
            .count(),
        0
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("workspace/writable"))
            .expect("payload did not run"),
        "original"
    );
}

#[tokio::test]
async fn immediate_payload_exit_preserves_status_and_every_output_byte() {
    // Arrange
    let fixture = Fixture::with_mode("exit").await;

    // Act
    let mut launch = Launch::start(&fixture.configuration, &fixture.probe, &fixture.entrypoint)
        .await
        .expect("completed setup");
    let status = tokio::time::timeout(Duration::from_secs(5), launch.child.wait())
        .await
        .expect("exit deadline")
        .expect("payload status");
    let mut output = String::new();
    launch
        .child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut output)
        .await
        .expect("payload output");

    // Assert
    assert_eq!(status.code(), Some(23));
    assert_eq!(output, "payload output\n");
}

#[tokio::test]
async fn invalid_entrypoint_acknowledgement_is_rejected() {
    // Arrange
    let fixture = Fixture::new().await;

    // Act
    let error = Launch::start(&fixture.configuration, &fixture.probe, &fixture.probe)
        .await
        .err()
        .expect("reject incorrect trusted helper");

    // Assert
    assert!(
        error.to_string().contains("invalid setup acknowledgement"),
        "{error}"
    );
    assert_eq!(
        fs::read_dir(fixture.configuration.scratch())
            .expect("scratch")
            .count(),
        0
    );
}

#[tokio::test]
async fn entrypoint_retries_interrupted_writes_and_never_falls_back_to_a_shell() {
    // Arrange
    let root = tempfile::tempdir().expect("owned fixture");
    let executable = root.path().join("entrypoint-test");
    build_fixture(
        "entrypoint_test.c",
        "AG_HARNESS_LINUX_ENTRYPOINT_TEST",
        &executable,
    )
    .await;

    // Act
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(executable).kill_on_drop(true).output(),
    )
    .await
    .expect("entrypoint unit deadline")
    .expect("entrypoint unit process");

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn entrypoint_requires_an_explicit_read_only_grant() {
    // Arrange
    let fixture = Fixture::new().await;

    // Act / Assert
    for path in [
        fixture.root.path().join("ungranted-entrypoint"),
        fixture.root.path().join("workspace/writable"),
    ] {
        fs::copy(&fixture.entrypoint, &path).expect("trusted entrypoint copy");
        let error = Launch::start(&fixture.configuration, &fixture.probe, &path)
            .await
            .err()
            .expect("reject missing read-only grant");
        assert!(
            error.to_string().contains("explicit read-only grant"),
            "{error}"
        );
        assert_eq!(
            fs::read_dir(fixture.configuration.scratch())
                .expect("scratch")
                .count(),
            0
        );
    }
}

#[tokio::test]
async fn payload_exec_failure_exits_inside_the_completed_sandbox() {
    // Arrange
    let fixture = Fixture::new().await;
    let wrapper = fixture.root.path().join("launcher");
    fs::copy(&fixture.probe, &wrapper).expect("retain trusted wrapper");
    fs::write(&fixture.probe, "invalid executable image").expect("payload exec failure");

    // Act
    let mut launch = Launch::start(&fixture.configuration, &wrapper, &fixture.entrypoint)
        .await
        .expect("isolation setup completed");
    let status = tokio::time::timeout(Duration::from_secs(5), launch.child.wait())
        .await
        .expect("exit deadline")
        .expect("payload exec status");
    let mut output = Vec::new();
    launch
        .child
        .stdout
        .take()
        .expect("stdout")
        .read_to_end(&mut output)
        .await
        .expect("remaining stdout");

    // Assert
    assert_eq!(status.code(), Some(126));
    assert!(
        output.is_empty(),
        "the acknowledgement is not payload output"
    );
}
