//! Native executor unit tests for identity, launcher encoding, notice
//! channels, and executable and filesystem inspection that fails closed.

use std::ffi::OsString;
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{
    Directory, Host, Inspection, LocalInspection, Native, NativeLaunch, NativeProcess,
    bounded_text, inspect_tree, probe, validate_executable,
};
use crate::bash::BashConfig;
use crate::command_journal::CommandCleanupScope;
use crate::execution::contract::{
    BashExecutor, BashProcess, Enforcement, ExecutionCommand, ExecutionError, ExecutionPolicy,
    Grants, MainExit, ProcessEvent,
};
use crate::execution::wire::Notice;
use crate::sandbox::SandboxUnavailableReason;

#[test]
fn native_executor_states_identity_enforcement_and_platform_cleanup_scope() {
    // Arrange
    let native = Native {
        configuration: process().configuration,
    };

    // Act / Assert
    assert_eq!(native.identity(), "native");
    assert_eq!(native.enforcement(), Enforcement::FileSystemAndNetwork);
    #[cfg(target_os = "linux")]
    assert_eq!(native.cleanup_scope(), CommandCleanupScope::PidNamespace);
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        native.cleanup_scope(),
        CommandCleanupScope::ProcessGroupBestEffort
    );
}

fn process() -> NativeProcess {
    NativeProcess {
        child: None,
        configuration: BashConfig::new(
            "/trusted/launcher".into(),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(1),
            1024,
        )
        .expect("config"),
        encoded: Vec::new(),
        finished: false,
        input: None,
        notice: Vec::new(),
        stderr: None,
        stdout: None,
    }
}

#[test]
fn launcher_encoding_rejects_non_utf8_arguments_and_environment() {
    // Arrange
    let invalid = OsString::from_vec(vec![0xff]);
    for (arguments, environment) in [
        (vec![invalid.clone()], vec![]),
        (vec![], vec![(invalid.clone(), "value".into())]),
        (vec![], vec![("NAME".into(), invalid)]),
    ] {
        let command = ExecutionCommand::new("/bin/bash".into(), arguments, ".".into())
            .expect("byte arguments");
        let policy = ExecutionPolicy::new(
            "/workspace".into(),
            vec!["/workspace/.git".into()],
            Grants {
                environment,
                ..Grants::default()
            },
        )
        .expect("byte environment");

        // Act / Assert
        assert!(matches!(
            process().launch(&command, &policy, vec![], vec![]),
            Err(ExecutionError::Setup)
        ));
    }
}

#[tokio::test]
async fn notice_channel_rejects_missing_oversized_invalid_and_out_of_order_messages() {
    // Arrange
    let mut process = process();

    // Act / Assert
    assert!(matches!(
        process.next_event(&mut []).await,
        Err(ExecutionError::Process)
    ));
    assert!(matches!(
        process.next_event(&mut [0; 16]).await,
        Err(ExecutionError::Process)
    ));
    for encoded in [
        b"invalid\n".to_vec(),
        vec![b' '; 257],
        b"\"Configure\"\n".to_vec(),
        b"\"Failed\"\n".to_vec(),
        vec![],
    ] {
        let (input, mut sender) = UnixStream::pair().expect("channel");
        process.input = Some(BufReader::new(input));
        process.notice.clear();
        sender.write_all(&encoded).await.expect("message");
        sender.shutdown().await.expect("EOF");
        assert!(matches!(
            process.next_event(&mut [0; 16]).await,
            Err(ExecutionError::Process)
        ));
    }
    process.cleanup().await.expect("nothing spawned");
}

#[tokio::test]
async fn notice_channel_preserves_main_exit_and_requires_completion_acknowledgment() {
    // Arrange
    let mut process = process();
    let (input, mut sender) = UnixStream::pair().expect("channel");
    process.input = Some(BufReader::new(input));

    // Act / Assert
    for (notice, expected) in [
        (
            Notice::MainExit {
                code: Some(7),
                signal: None,
            },
            MainExit::Code(7),
        ),
        (
            Notice::MainExit {
                code: None,
                signal: Some(15),
            },
            MainExit::Signal(15),
        ),
        (
            Notice::MainExit {
                code: None,
                signal: None,
            },
            MainExit::Unavailable,
        ),
    ] {
        let mut bytes = serde_json::to_vec(&notice).expect("notice");
        bytes.push(b'\n');
        sender.write_all(&bytes).await.expect("send");
        assert!(
            matches!(process.next_event(&mut [0; 16]).await, Ok(ProcessEvent::MainExit(exit)) if exit == expected)
        );
        assert!(!process.finished);
    }
    sender.write_all(b"\"Finished\"\n").await.expect("finish");
    assert!(matches!(
        process.next_event(&mut [0; 16]).await,
        Ok(ProcessEvent::Quiescent)
    ));
    assert!(process.finished);
    assert_eq!(sender.read_u8().await.expect("acknowledgment"), b'x');
}

#[tokio::test]
async fn executable_validation_rejects_missing_untrusted_nonexecutable_and_privileged_files() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let executable = root.join("executable");
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");

    // Act / Assert
    assert_eq!(
        validate_executable(&LocalInspection, &executable, &workspace).await,
        Err(ExecutionError::Unsupported)
    );
    std::fs::write(&executable, "#!/bin/sh\n").expect("executable");
    for mode in [0o600, 0o4700, 0o2700] {
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(mode)).expect("mode");
        assert_eq!(
            validate_executable(&LocalInspection, &executable, &workspace).await,
            Err(ExecutionError::Unsupported)
        );
    }
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).expect("mode");
    assert_eq!(
        validate_executable(&LocalInspection, &executable, &workspace).await,
        Ok(())
    );
    assert_eq!(
        validate_executable(&LocalInspection, &executable, &root).await,
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        validate_executable(&LocalInspection, &workspace, &root.join("other")).await,
        Err(ExecutionError::Unsupported)
    );
}

#[tokio::test]
async fn immutable_tree_aliases_are_bounded_and_cannot_grant_ipc() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let tree = root.join("tree");
    std::fs::create_dir(&tree).expect("tree");
    std::fs::write(tree.join("data"), "data").expect("data");
    symlink(tree.join("missing"), tree.join("dangling")).expect("dangling link");
    symlink(&tree, tree.join("cycle")).expect("cycle");
    let mut git = Vec::new();

    // Act / Assert
    assert_eq!(
        inspect_tree(&LocalInspection, &tree, &root, &mut git, false, 10).await,
        Ok(())
    );
    assert_eq!(
        inspect_tree(&LocalInspection, &tree, &root, &mut git, false, 1).await,
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        inspect_tree(
            &LocalInspection,
            &tree.join("cycle"),
            &root,
            &mut git,
            false,
            1
        )
        .await,
        Err(ExecutionError::Unsupported)
    );
    let socket = std::os::unix::net::UnixListener::bind(root.join("socket")).expect("socket");
    symlink(root.join("socket"), tree.join("ipc")).expect("socket alias");
    assert_eq!(
        inspect_tree(
            &LocalInspection,
            &tree.join("ipc"),
            &root,
            &mut git,
            false,
            10
        )
        .await,
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        inspect_tree(
            &LocalInspection,
            &tree.join("missing"),
            &root,
            &mut git,
            false,
            10
        )
        .await,
        Err(ExecutionError::Setup)
    );
    drop(socket);
}

#[tokio::test]
async fn metadata_indirections_reject_invalid_oversized_or_missing_targets() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let metadata = root.join(".git");
    let target = root.join("admin");
    std::fs::create_dir(&target).expect("admin");
    let mut git = Vec::new();

    // Act / Assert
    for (text, expected) in [
        ("x".repeat(4097), ExecutionError::Unsupported),
        ("invalid".into(), ExecutionError::Unsupported),
        ("gitdir: missing".into(), ExecutionError::Setup),
    ] {
        std::fs::write(&metadata, text).expect("metadata");
        assert_eq!(
            inspect_tree(&LocalInspection, &metadata, &root, &mut git, true, 10).await,
            Err(expected)
        );
    }
    std::fs::write(&metadata, "gitdir: admin").expect("gitdir");
    for (text, expected) in [
        ("x".repeat(4097), ExecutionError::Unsupported),
        ("missing".into(), ExecutionError::Setup),
    ] {
        std::fs::write(target.join("commondir"), text).expect("commondir");
        assert_eq!(
            inspect_tree(&LocalInspection, &metadata, &root, &mut git, true, 10).await,
            Err(expected)
        );
    }
    assert_eq!(
        bounded_text(&root.join("missing")).await,
        Err(ExecutionError::Setup)
    );
    std::fs::write(&metadata, [0xff]).expect("invalid UTF8");
    assert_eq!(bounded_text(&metadata).await, Err(ExecutionError::Setup));
    std::fs::write(&metadata, vec![b'x'; 8192]).expect("oversized file");
    assert_eq!(
        bounded_text(&metadata).await.expect("bounded read").len(),
        4097
    );
}

#[tokio::test]
async fn unavailable_launcher_never_creates_a_child() {
    // Arrange
    let mut process = process();
    process.configuration.snapshot.launcher = Some("/missing/ag-harness-launcher".into());

    // Act / Assert
    assert_eq!(process.start().await, Err(ExecutionError::Setup));
    assert!(process.child.is_none());
    process.cleanup().await.expect("cleanup without child");
}

#[tokio::test]
async fn unreadable_directories_and_cyclic_links_fail_closed() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("root");
    let tree = root.join("unreadable");
    std::fs::create_dir(&tree).expect("tree");
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o000)).expect("restrict");
    let mut git = Vec::new();

    // Act
    let unreadable = inspect_tree(&LocalInspection, &tree, &root, &mut git, false, 10).await;
    std::fs::set_permissions(&tree, std::fs::Permissions::from_mode(0o700)).expect("restore");
    symlink(root.join("loop"), root.join("loop")).expect("cyclic link");
    let cyclic = inspect_tree(
        &LocalInspection,
        &root.join("loop"),
        &root,
        &mut git,
        false,
        10,
    )
    .await;

    // Assert
    assert_eq!(unreadable, Err(ExecutionError::Setup));
    assert_eq!(cyclic, Err(ExecutionError::Setup));
}

struct FailingInspection {
    operation: &'static str,
    remaining: AtomicUsize,
}

impl FailingInspection {
    fn new(operation: &'static str, occurrence: usize) -> Self {
        Self {
            operation,
            remaining: AtomicUsize::new(occurrence),
        }
    }

    fn check(&self, operation: &str) -> io::Result<()> {
        if self.operation == operation && self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Ok(())
    }
}

#[async_trait]
impl Inspection for FailingInspection {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.check("canonicalize")?;
        LocalInspection.canonicalize(path).await
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.check("metadata")?;
        LocalInspection.metadata(path).await
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        self.check("exists")?;
        LocalInspection.try_exists(path).await
    }

    async fn read_dir(&self, path: &Path) -> io::Result<Box<dyn Directory>> {
        if self.operation == "entry" {
            return Ok(Box::new(FailingDirectory));
        }
        LocalInspection.read_dir(path).await
    }
}

struct FailingDirectory;

#[async_trait]
impl Directory for FailingDirectory {
    async fn next_entry(&mut self) -> io::Result<Option<PathBuf>> {
        Err(io::ErrorKind::PermissionDenied.into())
    }
}

#[tokio::test]
async fn inspection_failure_between_observations_never_grants_access() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("root");
    let executable = root.join("executable");
    std::fs::write(&executable, "#!/bin/sh\n").expect("executable");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).expect("mode");
    let workspace = root.join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    symlink(&workspace, root.join("alias")).expect("directory alias");
    symlink(&executable, root.join("file-alias")).expect("file alias");
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}", root.display()),
    )
    .expect("gitdir");
    let mut git = Vec::new();

    // Act / Assert
    for (operation, occurrence) in [("canonicalize", 2), ("metadata", 1)] {
        let files = FailingInspection::new(operation, occurrence);
        assert_eq!(
            validate_executable(&files, &executable, &workspace).await,
            Err(ExecutionError::Unsupported)
        );
    }
    for (operation, path) in [
        ("canonicalize", root.join("alias")),
        ("entry", workspace.clone()),
        ("exists", workspace.join(".git")),
    ] {
        let files = FailingInspection::new(operation, 1);
        assert_eq!(
            inspect_tree(&files, &path, &workspace, &mut git, false, 10).await,
            Err(ExecutionError::Setup)
        );
    }
    assert_eq!(
        inspect_tree(
            &LocalInspection,
            &root.join("file-alias"),
            &workspace,
            &mut git,
            false,
            10
        )
        .await,
        Ok(())
    );
    assert_eq!(
        inspect_tree(
            &LocalInspection,
            &workspace.join(".git"),
            &workspace,
            &mut git,
            false,
            10
        )
        .await,
        Ok(())
    );
    assert!(git.contains(&root));
}

#[test]
fn unqualified_platform_cannot_bind_native_resources() {
    // Arrange
    let backend = Native {
        configuration: process().configuration,
    };

    // Act / Assert
    assert!(matches!(
        backend.bind_for_platform("unsupported"),
        Err(ExecutionError::Unsupported)
    ));
    for platform in ["linux", "macos"] {
        assert!(backend.bind_for_platform(platform).is_ok());
    }
}

#[tokio::test]
async fn probe_reports_the_first_missing_native_sandbox_piece() {
    // Arrange
    let fixture = ProbeFixture::new();
    let missing = ProbeFixture::configuration(&fixture.tools.join("missing-bwrap"));
    let failing = ProbeFixture::configuration(&fixture.script("failing-bwrap", 1));
    let writes = failing.clone().with_write("output".into()).expect("write");
    let inside = fixture.workspace.join("helper");
    std::fs::copy(&fixture.helper, &inside).expect("helper inside workspace");
    let missing_seatbelt = fixture.tools.join("missing-sandbox-exec");
    let failing_seatbelt = fixture.script("failing-sandbox-exec", 1);

    // Act
    let reasons = [
        probe(
            &missing,
            Some(&fixture.helper),
            &fixture.workspace,
            host("freebsd", true, &missing_seatbelt),
        )
        .await,
        probe(
            &missing,
            None,
            &fixture.workspace,
            host("linux", true, &missing_seatbelt),
        )
        .await,
        probe(
            &missing,
            Some(&inside),
            &fixture.workspace,
            host("linux", true, &missing_seatbelt),
        )
        .await,
        probe(
            &writes,
            Some(&fixture.helper),
            &fixture.workspace,
            host("linux", false, &missing_seatbelt),
        )
        .await,
        probe(
            &missing,
            Some(&fixture.helper),
            &fixture.workspace,
            host("linux", true, &missing_seatbelt),
        )
        .await,
        probe(
            &failing,
            Some(&fixture.helper),
            &fixture.workspace,
            host("linux", true, &missing_seatbelt),
        )
        .await,
        probe(
            &missing,
            Some(&fixture.helper),
            &fixture.workspace,
            host("macos", true, &missing_seatbelt),
        )
        .await,
        probe(
            &missing,
            Some(&fixture.helper),
            &fixture.workspace,
            host("macos", true, &failing_seatbelt),
        )
        .await,
    ];

    // Assert
    assert_eq!(
        reasons,
        [
            Err(SandboxUnavailableReason::UnsupportedPlatform),
            Err(SandboxUnavailableReason::HelperMissing),
            Err(SandboxUnavailableReason::HelperUntrusted),
            Err(SandboxUnavailableReason::LandlockUnavailable),
            Err(SandboxUnavailableReason::BubblewrapMissing),
            Err(SandboxUnavailableReason::UserNamespacesUnavailable),
            Err(SandboxUnavailableReason::SeatbeltMissing),
            Err(SandboxUnavailableReason::SeatbeltUnavailable),
        ]
    );
}

#[tokio::test]
async fn probe_leaves_a_bash_the_launch_rejects_to_fail_closed_there() {
    // Arrange
    let fixture = ProbeFixture::new();
    let failing = fixture.script("failing-sandbox", 1);
    let configuration = BashConfig::native(
        fixture.workspace.join("bash"),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("config")
    .with_host_information()
    .with_linux_bubblewrap(failing.clone())
    .expect("bubblewrap");

    // Act
    let linux = probe(
        &configuration,
        Some(&fixture.helper),
        &fixture.workspace,
        host("linux", true, &failing),
    )
    .await;
    let macos = probe(
        &configuration,
        Some(&fixture.helper),
        &fixture.workspace,
        host("macos", true, &failing),
    )
    .await;

    // Assert
    assert_eq!(
        linux,
        Ok(NativeLaunch {
            bubblewrap: Some(failing),
            launcher: fixture.helper.clone(),
        })
    );
    assert_eq!(
        macos,
        Ok(NativeLaunch {
            bubblewrap: None,
            launcher: fixture.helper.clone(),
        })
    );
}

#[tokio::test]
async fn probe_returns_the_executables_an_available_sandbox_launches_through() {
    // Arrange
    let fixture = ProbeFixture::new();
    let bubblewrap = fixture.script("bwrap", 0);
    let seatbelt = fixture.script("sandbox-exec", 0);
    let configuration = ProbeFixture::configuration(&bubblewrap)
        .with_write("output".into())
        .expect("write");

    // Act
    let linux = probe(
        &configuration,
        Some(&fixture.helper),
        &fixture.workspace,
        host("linux", true, &seatbelt),
    )
    .await;
    let macos = probe(
        &configuration,
        Some(&fixture.helper),
        &fixture.workspace,
        host("macos", false, &seatbelt),
    )
    .await;

    // Assert
    assert_eq!(
        linux,
        Ok(NativeLaunch {
            bubblewrap: Some(bubblewrap),
            launcher: fixture.helper.clone(),
        })
    );
    assert_eq!(
        macos,
        Ok(NativeLaunch {
            bubblewrap: None,
            launcher: fixture.helper.clone(),
        })
    );
}

#[tokio::test]
async fn probe_defaults_to_the_system_bubblewrap() {
    // Arrange
    let fixture = ProbeFixture::new();
    let configuration = BashConfig::native(
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("config")
    .with_host_information();

    // Act
    let checked = probe(
        &configuration,
        Some(&fixture.helper),
        &fixture.workspace,
        host("linux", true, &fixture.helper),
    )
    .await;

    // Assert
    // Linux test hosts install a working system Bubblewrap; macOS has none.
    #[cfg(target_os = "linux")]
    assert_eq!(
        checked.map(|launch| launch.bubblewrap),
        Ok(Some(PathBuf::from("/usr/bin/bwrap")))
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(checked, Err(SandboxUnavailableReason::BubblewrapMissing));
}

#[test]
fn current_host_names_this_platform_and_the_system_seatbelt() {
    // Act
    let host = Host::current();

    // Assert
    assert_eq!(host.platform, std::env::consts::OS);
    assert_eq!(host.seatbelt, Path::new("/usr/bin/sandbox-exec"));
    #[cfg(not(target_os = "linux"))]
    assert!(!host.landlock);
}

/// A workspace beside a directory of trusted executables.
struct ProbeFixture {
    _directory: tempfile::TempDir,
    helper: PathBuf,
    tools: PathBuf,
    workspace: PathBuf,
}

impl ProbeFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path().canonicalize().expect("root");
        let tools = root.join("tools");
        let workspace = root.join("workspace");
        std::fs::create_dir(&tools).expect("tools");
        std::fs::create_dir_all(workspace.join("output")).expect("workspace");
        let mut fixture = Self {
            _directory: directory,
            helper: PathBuf::new(),
            tools,
            workspace,
        };
        fixture.helper = fixture.script("helper", 0);

        fixture
    }

    /// Writes an executable script exiting with `code` among the tools.
    fn script(&self, name: &str, code: u8) -> PathBuf {
        let path = self.tools.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexit {code}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("executable script");

        path
    }

    /// Returns a native configuration launching through `bubblewrap`.
    fn configuration(bubblewrap: &Path) -> BashConfig {
        BashConfig::native(
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(1),
            1024,
        )
        .expect("config")
        .with_host_information()
        .with_linux_bubblewrap(bubblewrap.to_path_buf())
        .expect("bubblewrap")
    }
}

fn host(platform: &'static str, landlock: bool, seatbelt: &Path) -> Host {
    Host {
        landlock,
        platform,
        seatbelt: seatbelt.to_path_buf(),
    }
}
