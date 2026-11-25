//! Native executor unit tests for identity, launcher encoding, notice
//! channels, and executable and filesystem inspection that fails closed.

use std::ffi::OsString;
use std::fs::Metadata;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{
    Access, Host, HostCacheLimits, Inspection, LocalInspection, Native, NativeLaunch,
    NativeProcess, allowed_root, bounded_bytes, git_directories, pointer, probe, resolve_access,
    resolve_hidden, validate_executable,
};
use crate::bash::{BashConfig, HostCachePin, HostCachePins};
use crate::command_journal::CommandCleanupScope;
use crate::execution::contract::{
    BashExecutor, BashProcess, Enforcement, ExecutionCommand, ExecutionError, ExecutionPolicy,
    Grants, MainExit, ProcessEvent, ReadScope,
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
    assert!(native.supports_hidden_paths());
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
            process().launch(&command, &policy, Access::default()),
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
async fn git_directories_follow_the_workspace_entry_and_its_common_directory() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let workspace = root.join("workspace");
    let admin = root.join("common/worktrees/session");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::create_dir_all(&admin).expect("worktree administration");

    // Act / Assert
    assert!(
        git_directories(&LocalInspection, &workspace)
            .await
            .expect("no repository")
            .is_none()
    );
    std::fs::create_dir(workspace.join(".git")).expect("Git directory");
    let own = git_directories(&LocalInspection, &workspace)
        .await
        .expect("repository")
        .expect("Git directory");
    assert_eq!(own.admin, workspace.join(".git"));
    assert_eq!(own.common, workspace.join(".git"));
    std::fs::remove_dir(workspace.join(".git")).expect("replace with pointer");
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("gitdir pointer");
    std::fs::write(admin.join("commondir"), "../..\n").expect("common pointer");
    let linked = git_directories(&LocalInspection, &workspace)
        .await
        .expect("linked worktree")
        .expect("Git directories");
    assert_eq!(linked.admin, admin);
    assert_eq!(linked.common, root.join("common"));
}

#[tokio::test]
async fn git_pointers_reject_invalid_oversized_or_missing_targets() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let metadata = root.join(".git");
    let target = root.join("admin");
    std::fs::create_dir(&target).expect("admin");

    // Act / Assert
    for (text, expected) in [
        ("x".repeat(4097).into_bytes(), ExecutionError::Unsupported),
        (b"invalid".to_vec(), ExecutionError::Unsupported),
        (vec![0xff], ExecutionError::Unsupported),
        (b"gitdir: missing".to_vec(), ExecutionError::Setup),
    ] {
        std::fs::write(&metadata, text).expect("metadata");
        assert_eq!(
            git_directories(&LocalInspection, &root).await.map(|_| ()),
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
            git_directories(&LocalInspection, &root).await.map(|_| ()),
            Err(expected)
        );
    }
    assert_eq!(
        pointer(&root.join("missing")).await,
        Err(ExecutionError::Setup)
    );
    assert_eq!(
        bounded_bytes(&target.join("commondir"), 7).await,
        Ok(b"missing".to_vec())
    );
    assert_eq!(
        bounded_bytes(&target.join("commondir"), 6).await,
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        bounded_bytes(&target, 6).await,
        Err(ExecutionError::Setup),
        "a directory opens but cannot be read"
    );
}

/// A workspace with a linked worktree's `.git` pointer beside its common
/// directory, an outside directory, and a temporary directory.
struct AccessFixture {
    _directory: tempfile::TempDir,
    admin: PathBuf,
    common: PathBuf,
    outside: PathBuf,
    temporary: PathBuf,
    workspace: PathBuf,
}

impl AccessFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path().canonicalize().expect("canonical");
        let workspace = root.join("workspace");
        let common = root.join("common");
        let admin = common.join("worktrees/session");
        let outside = root.join("outside");
        let temporary = root.join("temporary");
        for path in [&workspace, &admin, &outside, &temporary] {
            std::fs::create_dir_all(path).expect("fixture directory");
        }
        std::fs::create_dir(workspace.join("output")).expect("write grant");
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .expect("gitdir pointer");
        std::fs::write(admin.join("commondir"), "../..\n").expect("common pointer");

        Self {
            _directory: directory,
            admin,
            common,
            outside,
            temporary,
            workspace,
        }
    }

    fn policy(&self, grants: Grants) -> ExecutionPolicy {
        ExecutionPolicy::new(
            self.workspace.clone(),
            vec![self.workspace.join(".git")],
            grants,
        )
        .expect("policy")
    }
}

#[tokio::test]
async fn access_resolves_grants_by_path_and_keeps_the_repository_git_directories_read_only() {
    // Arrange
    let fixture = AccessFixture::new();
    symlink(&fixture.outside, fixture.temporary.join("alias")).expect("read alias");
    let policy = fixture.policy(Grants {
        external_reads: vec![fixture.outside.clone(), fixture.temporary.join("alias")],
        temporary_directory: Some(fixture.temporary.clone()),
        workspace_writes: vec!["output".into()],
        ..Grants::default()
    });

    // Act
    let access = resolve_access(&LocalInspection, &policy)
        .await
        .expect("access");

    // Assert
    assert_eq!(
        access.external_reads,
        [
            fixture.outside.clone(),
            fixture.temporary.join("alias"),
            fixture.outside.clone(),
            fixture.admin.clone(),
            fixture.common.clone(),
        ]
    );
    assert_eq!(
        access.git_metadata,
        [
            fixture.workspace.join(".git"),
            fixture.admin.clone(),
            fixture.common.clone(),
            fixture.workspace.join("output/.git"),
        ]
    );
    assert_eq!(
        access.writes,
        [fixture.workspace.join("output"), fixture.temporary.clone()]
    );
    assert_eq!(access.write_nodes.len(), 2);
}

#[tokio::test]
async fn access_in_a_workspace_without_a_repository_protects_only_its_git_name() {
    // Arrange
    let directory = tempfile::tempdir().expect("repository-free workspace");
    let workspace = directory.path().canonicalize().expect("canonical");
    let policy = ExecutionPolicy::new(
        workspace.clone(),
        vec![workspace.join(".git")],
        Grants::default(),
    )
    .expect("policy");

    // Act
    let access = resolve_access(&LocalInspection, &policy).await;

    // Assert
    assert_eq!(
        access.map(|access| access.git_metadata),
        Ok(vec![workspace.join(".git")])
    );
}

#[tokio::test]
async fn access_refuses_grants_the_sandbox_cannot_honor() {
    // Arrange
    let fixture = AccessFixture::new();
    symlink(&fixture.workspace, fixture.outside.join("workspace-alias")).expect("alias");
    symlink(
        fixture.workspace.join("output"),
        fixture.workspace.join("linked-output"),
    )
    .expect("write alias");
    std::fs::write(fixture.workspace.join("file"), "file").expect("file");
    let cases = [
        Grants {
            external_reads: vec![fixture.outside.join("missing")],
            ..Grants::default()
        },
        Grants {
            external_reads: vec!["/".into()],
            ..Grants::default()
        },
        Grants {
            external_reads: vec![fixture.outside.join("workspace-alias")],
            ..Grants::default()
        },
        Grants {
            workspace_writes: vec!["missing".into()],
            ..Grants::default()
        },
        Grants {
            workspace_writes: vec!["linked-output".into()],
            ..Grants::default()
        },
        Grants {
            workspace_writes: vec!["file".into()],
            ..Grants::default()
        },
        Grants {
            temporary_directory: Some(fixture.outside.join("workspace-alias/output")),
            ..Grants::default()
        },
        Grants {
            temporary_directory: Some(fixture.common.join("objects-alias")),
            ..Grants::default()
        },
    ];
    symlink(&fixture.admin, fixture.common.join("objects-alias")).expect("metadata alias");

    // Act / Assert
    for grants in cases {
        assert_eq!(
            resolve_access(&LocalInspection, &fixture.policy(grants))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    assert_eq!(
        resolve_access(
            &FailingInspection::new("metadata", 1),
            &fixture.policy(Grants::default())
        )
        .await
        .map(|_| ()),
        Err(ExecutionError::Setup)
    );
    // The repository resolves through two canonicalizations; the read
    // grant's own comes third and its parent's fourth.
    assert_eq!(
        resolve_access(
            &FailingInspection::new("canonicalize", 4),
            &fixture.policy(Grants {
                external_reads: vec![fixture.outside.clone()],
                ..Grants::default()
            })
        )
        .await
        .map(|_| ()),
        Err(ExecutionError::Unsupported)
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

    async fn home(&self) -> Option<PathBuf> {
        LocalInspection.home().await
    }

    async fn host_variable(&self, name: &str) -> Option<std::ffi::OsString> {
        LocalInspection.host_variable(name).await
    }
}

/// Local inspection with another account home directory and process
/// environment.
struct HomeInspection {
    account: Option<PathBuf>,
    host: Vec<(&'static str, PathBuf)>,
}

impl HomeInspection {
    /// Returns an inspection whose account home and `HOME` are `home`.
    fn at(home: &Path) -> Self {
        Self {
            account: Some(home.to_path_buf()),
            host: vec![("HOME", home.to_path_buf())],
        }
    }
}

#[async_trait]
impl Inspection for HomeInspection {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        LocalInspection.canonicalize(path).await
    }

    async fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        LocalInspection.metadata(path).await
    }

    async fn try_exists(&self, path: &Path) -> io::Result<bool> {
        LocalInspection.try_exists(path).await
    }

    async fn home(&self) -> Option<PathBuf> {
        self.account.clone()
    }

    async fn host_variable(&self, name: &str) -> Option<std::ffi::OsString> {
        self.host
            .iter()
            .find(|(variable, _)| *variable == name)
            .map(|(_, value)| value.clone().into_os_string())
    }
}

#[tokio::test]
async fn access_resolves_writable_roots_hidden_paths_and_sockets() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let host_home = fixture.outside.join("host-home");
    let registry = fixture.outside.join("shared-cache");
    let sessions = fixture.outside.join("sessions");
    let hidden_file = fixture.outside.join("hidden");
    for directory in [
        &registry,
        &host_home,
        &sessions.join("own"),
        &sessions.join("other"),
    ] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }
    std::fs::write(&hidden_file, "hidden").expect("hidden file");
    symlink(&hidden_file, fixture.outside.join("hidden-alias")).expect("hidden alias");
    let socket = fixture.outside.join("agent.sock");
    let policy = fixture.policy(Grants {
        environment: vec![
            // A command `HOME` inside the session belongs to the session.
            ("HOME".into(), sessions.join("own/home").into_os_string()),
        ],
        hidden_paths: vec![
            fixture.outside.join("hidden-alias"),
            fixture.outside.join("missing/key"),
        ],
        read_scope: ReadScope::FullDisk,
        temporary_directory: Some(sessions.join("own")),
        unix_sockets: vec![socket.clone()],
        writable_roots: vec![registry.clone()],
        ..Grants::default()
    });
    let files = HomeInspection {
        account: Some(home.clone()),
        host: vec![("HOME", host_home.clone())],
    };

    // Act
    let access = resolve_access(&files, &policy).await.expect("access");

    // Assert
    assert_eq!(access.writes, [sessions.join("own"), registry]);
    assert_eq!(access.write_nodes.len(), 2);
    // Links resolve, and a missing path keeps its name beneath its nearest
    // existing ancestor; full-disk reads add the credential stores in both
    // the account home and the different `HOME`, and the other sessions'
    // directories.
    for path in [
        hidden_file,
        fixture.outside.join("missing/key"),
        sessions.clone(),
    ] {
        assert!(access.hidden_paths.contains(&path), "{}", path.display());
    }
    for store in [
        "Library/Keychains",
        ".ssh",
        ".config/gh",
        ".git-credentials",
        ".npmrc",
        ".cargo/credentials.toml",
        ".pypirc",
        ".codex/auth.json",
        ".claude/.credentials.json",
        ".mozilla/firefox",
        "Library/Application Support/Google/Chrome",
        ".config/BraveSoftware",
        ".thunderbird",
        ".m2/settings.xml",
        ".gradle/gradle.properties",
        ".zsh_history",
    ] {
        for home in [&home, &host_home] {
            assert!(access.hidden_paths.contains(&home.join(store)), "{store}");
        }
    }
    assert!(
        !access
            .hidden_paths
            .iter()
            .any(|path| path.starts_with(sessions.join("own")))
    );
    assert!(
        access
            .hidden_paths
            .contains(&resolve_hidden(&LocalInspection, Path::new("/run/secrets")).await)
    );
    assert_eq!(access.unix_sockets, [socket]);
}

#[tokio::test]
async fn full_disk_reads_hide_stores_the_environment_moves() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let relocated = fixture.outside.join("relocated");
    let sessions = fixture.outside.join("sessions");
    std::fs::create_dir_all(sessions.join("own")).expect("own session");
    let policy = fixture.policy(Grants {
        environment: vec![
            // A store inside the session's own directory belongs to it.
            (
                "CARGO_HOME".into(),
                sessions.join("own/cargo").into_os_string(),
            ),
            (
                "KUBECONFIG".into(),
                relocated.join("kube-a:").into_os_string(),
            ),
        ],
        read_scope: ReadScope::FullDisk,
        temporary_directory: Some(sessions.join("own")),
        ..Grants::default()
    });
    let files = HomeInspection {
        account: Some(home),
        host: vec![
            ("XDG_CONFIG_HOME", relocated.join("config")),
            ("XDG_DATA_HOME", relocated.join("data")),
            ("CARGO_HOME", relocated.join("cargo")),
            ("GNUPGHOME", relocated.join("gnupg")),
            ("CODEX_HOME", relocated.join("codex")),
            ("HISTFILE", relocated.join("history")),
        ],
    };

    // Act
    let access = resolve_access(&files, &policy).await.expect("access");

    // Assert
    for store in [
        "config/gh",
        "config/git/credentials",
        "config/BraveSoftware",
        "data/fish/fish_history",
        "cargo/credentials.toml",
        "gnupg",
        "codex/auth.json",
        "history",
        "kube-a",
    ] {
        assert!(
            access.hidden_paths.contains(&relocated.join(store)),
            "{store}"
        );
    }
    assert!(
        !access
            .hidden_paths
            .iter()
            .any(|path| path.starts_with(sessions.join("own")))
    );
    // A store that contains the workspace could not be hidden.
    let around_workspace = HomeInspection {
        account: Some(fixture.outside.join("home")),
        host: vec![("GNUPGHOME", fixture.workspace.clone())],
    };
    assert_eq!(
        resolve_access(&around_workspace, &policy).await.map(|_| ()),
        Err(ExecutionError::Unsupported)
    );
}

#[tokio::test]
async fn full_disk_reads_refuse_temporary_parents_they_cannot_hide() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    std::fs::create_dir_all(home.join("session")).expect("home session");

    // Act / Assert
    // Beside the workspace, or in a parent containing the home directory,
    // the other entries could not be hidden.
    for temporary in [fixture.temporary.clone(), home.join("session")] {
        let policy = fixture.policy(Grants {
            read_scope: ReadScope::FullDisk,
            temporary_directory: Some(temporary.clone()),
            ..Grants::default()
        });
        assert_eq!(
            resolve_access(&HomeInspection::at(&home), &policy)
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{temporary:?}"
        );
    }
}

#[tokio::test]
async fn an_unknown_account_home_refuses_full_disk_reads_and_writable_roots() {
    // Arrange
    let fixture = AccessFixture::new();
    std::fs::create_dir_all(fixture.outside.join("cache")).expect("cache");
    let files = HomeInspection {
        account: None,
        host: Vec::new(),
    };

    // Act / Assert
    for grants in [
        Grants {
            read_scope: ReadScope::FullDisk,
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.join("cache")],
            ..Grants::default()
        },
    ] {
        assert_eq!(
            resolve_access(&files, &fixture.policy(grants))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    assert!(
        resolve_access(&files, &fixture.policy(Grants::default()))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn access_refuses_coding_grants_the_sandbox_cannot_honor() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let tool_cache = fixture.outside.join("shared-cache");
    for directory in [
        &tool_cache,
        &home.join(".npm/_cacache"),
        &home.join(".cargo/registry/cache"),
        &home.join(".cache/uv"),
    ] {
        std::fs::create_dir_all(directory).expect("cache directory");
    }
    std::fs::write(fixture.outside.join("file"), "file").expect("file");
    symlink(&fixture.workspace, fixture.outside.join("workspace-alias")).expect("alias");
    symlink(&fixture.admin, fixture.outside.join("metadata-alias")).expect("metadata alias");
    symlink(&tool_cache, fixture.outside.join("cache-alias")).expect("cache alias");
    let in_cache = |grants: Grants| Grants {
        writable_roots: vec![tool_cache.clone()],
        ..grants
    };
    let cases = [
        // Writable roots overlapping a home directory, even tool caches
        // host tools use without re-verifying them.
        Grants {
            writable_roots: vec![home.clone()],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.clone()],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".npm/_cacache")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".cargo/registry/cache")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".cache/uv")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.join("file")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.join("workspace-alias")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.join("metadata-alias")],
            ..Grants::default()
        },
        // A hidden path containing the workspace, or inside a writable
        // directory once resolved, where renaming a parent would reveal it.
        Grants {
            hidden_paths: vec![fixture.outside.join("workspace-alias")],
            ..Grants::default()
        },
        in_cache(Grants {
            hidden_paths: vec![fixture.outside.join("cache-alias/secret")],
            ..Grants::default()
        }),
        // A socket a command could replace.
        in_cache(Grants {
            unix_sockets: vec![tool_cache.join("agent.sock")],
            ..Grants::default()
        }),
        Grants {
            unix_sockets: vec![fixture.workspace.join("agent.sock")],
            ..Grants::default()
        },
    ];

    // Act / Assert
    for grants in cases {
        assert_eq!(
            resolve_access(&HomeInspection::at(&home), &fixture.policy(grants))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    let policy = fixture.policy(in_cache(Grants::default()));
    assert!(
        resolve_access(&HomeInspection::at(&home), &policy)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn access_refuses_roots_given_by_links_or_in_protected_places() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let cache = fixture.outside.join("shared-cache");
    let session_temporary = home.join(".host/session/tmp");
    for directory in [
        &cache.join("inner"),
        &home.join("Library/LaunchAgents"),
        &session_temporary,
    ] {
        std::fs::create_dir_all(directory).expect("directory");
    }
    symlink(&cache, fixture.outside.join("cache-alias")).expect("cache alias");
    let cases = [
        // A root or temporary directory given by a link in its place.
        Grants {
            writable_roots: vec![fixture.outside.join("cache-alias")],
            ..Grants::default()
        },
        Grants {
            temporary_directory: Some(fixture.outside.join("cache-alias")),
            ..Grants::default()
        },
        // A temporary directory in a home's `Library`, containing a home,
        // or in a system directory.
        Grants {
            temporary_directory: Some(home.join("Library/LaunchAgents")),
            ..Grants::default()
        },
        Grants {
            temporary_directory: Some(fixture.outside.clone()),
            ..Grants::default()
        },
        Grants {
            temporary_directory: Some("/usr".into()),
            ..Grants::default()
        },
        // Grants nested in one another outside the workspace.
        Grants {
            temporary_directory: Some(cache.join("inner")),
            writable_roots: vec![cache.clone()],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![cache.clone(), cache.join("inner")],
            ..Grants::default()
        },
    ];

    // Act / Assert
    for grants in cases {
        assert_eq!(
            resolve_access(&HomeInspection::at(&home), &fixture.policy(grants))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    // A host's per-session temporary directory usually lies in its home.
    let in_home = fixture.policy(Grants {
        temporary_directory: Some(session_temporary),
        writable_roots: vec![cache],
        ..Grants::default()
    });
    assert!(
        resolve_access(&HomeInspection::at(&home), &in_home)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn hidden_paths_resolve_to_the_exact_names_the_sandbox_matches() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    std::fs::write(root.join("file"), "secret").expect("file");
    symlink(root.join("file"), root.join("alias")).expect("alias");

    // Act
    let existing = resolve_hidden(&LocalInspection, &root.join("alias")).await;
    let missing = resolve_hidden(&LocalInspection, &root.join("missing/key")).await;

    // Assert
    // Compared as text: a trailing separator would no longer name the file
    // for Linux, though paths compare equal by components.
    assert_eq!(existing.as_os_str(), root.join("file").as_os_str());
    assert_eq!(missing.as_os_str(), root.join("missing/key").as_os_str());
}

#[test]
fn writable_roots_stay_outside_home_and_system_directories() {
    // Arrange
    let homes = [PathBuf::from("/home/user"), PathBuf::from("/srv/home")];
    let cases = [
        ("/private/var/folders/x/T/session-cache", true),
        ("/mnt/build-cache", true),
        // Anything in a home directory, even caches tools claim to verify:
        // Cargo, Go, and Git use cached entries without re-verifying them.
        ("/home/user/.cargo/registry/cache", false),
        ("/home/user/.cargo/git/db", false),
        ("/home/user/.npm/_cacache", false),
        ("/home/user/go/pkg/mod/cache/download", false),
        ("/srv/home/.npm/_cacache", false),
        ("/home/user/.cargo/registry", false),
        ("/home/user/.cargo/registry/src", false),
        ("/home/user/.cargo/registry/index", false),
        ("/home/user/go/pkg/mod", false),
        ("/home/user/.cache/sccache", false),
        ("/home/user/.cache/go-build", false),
        ("/home/user/.cache/bazel", false),
        ("/home/user/.gradle/caches", false),
        ("/home/user/.cache/uv", false),
        ("/home/user/.cache/pypoetry/virtualenvs", false),
        ("/home/user/.cache/node/corepack", false),
        ("/home/user/.cache/deno", false),
        ("/home/user/.cache/puppeteer", false),
        ("/home/user/.cache/selenium", false),
        ("/home/user/Library/Caches/Cypress", false),
        ("/home/user/Library/Caches/Homebrew", false),
        ("/home/user", false),
        ("/home/user/.cargo", false),
        ("/home/user/.cargo/bin", false),
        ("/home/user/.config", false),
        ("/home/user/.local/bin", false),
        ("/srv/home/.config", false),
        ("/srv", false),
        ("/", false),
        ("/opt/homebrew", false),
        ("/home/linuxbrew/.linuxbrew", false),
        ("/usr/local", false),
        ("/etc", false),
        ("/Library/Caches", false),
        ("/var", false),
    ];

    // Act / Assert
    for (root, expected) in cases {
        assert_eq!(allowed_root(Path::new(root), &homes), expected, "{root}");
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
    std::fs::write(
        workspace.join(".git"),
        format!("gitdir: {}", root.display()),
    )
    .expect("gitdir");
    std::fs::write(root.join("commondir"), ".").expect("common pointer");

    // Act / Assert
    for (operation, occurrence) in [("canonicalize", 2), ("metadata", 1)] {
        let files = FailingInspection::new(operation, occurrence);
        assert_eq!(
            validate_executable(&files, &executable, &workspace).await,
            Err(ExecutionError::Unsupported)
        );
    }
    for (operation, occurrence) in [("canonicalize", 1), ("canonicalize", 2), ("exists", 1)] {
        let files = FailingInspection::new(operation, occurrence);
        assert_eq!(
            git_directories(&files, &workspace).await.map(|_| ()),
            Err(ExecutionError::Setup),
            "{operation} {occurrence}"
        );
    }
    let resolved = git_directories(&LocalInspection, &workspace)
        .await
        .expect("repository")
        .expect("Git directories");
    assert_eq!(resolved.admin, root);
    assert_eq!(resolved.common, root);
    let policy = ExecutionPolicy::new(
        workspace.clone(),
        vec![workspace.join(".git")],
        Grants {
            workspace_writes: vec![".".into()],
            ..Grants::default()
        },
    )
    .expect("policy");
    for operation in ["canonicalize", "metadata"] {
        let occurrence = if operation == "canonicalize" { 3 } else { 2 };
        assert_eq!(
            resolve_access(&FailingInspection::new(operation, occurrence), &policy)
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{operation}"
        );
    }
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
    let temporary = failing
        .clone()
        .with_temporary_directory(fixture.tools.clone())
        .expect("temporary directory");
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
            &temporary,
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

/// A fake home with Cargo's, npm's, and Go's caches beside Cargo's
/// credentials and the program directories on `PATH`.
struct CacheHome {
    fixture: AccessFixture,
    home: PathBuf,
}

impl CacheHome {
    fn new() -> Self {
        let fixture = AccessFixture::new();
        let home = fixture.outside.join("home");
        for directory in [
            ".cargo/registry",
            ".cargo/git",
            ".cargo/bin",
            ".npm",
            "go/bin",
            "go/pkg/mod",
            "repository/.git/objects",
            "sessions/own",
        ] {
            std::fs::create_dir_all(home.join(directory)).expect("cache home directory");
        }
        for file in [".cargo/.package-cache", ".cargo/credentials.toml"] {
            std::fs::write(home.join(file), "").expect("cache home file");
        }

        Self { fixture, home }
    }

    /// Returns an inspection whose account home and `HOME` are the fake
    /// home and whose `PATH` holds its program directories.
    fn files(&self) -> HomeInspection {
        let mut files = HomeInspection::at(&self.home);
        let programs =
            std::env::join_paths([self.home.join(".cargo/bin"), self.home.join("go/bin")])
                .expect("program path");
        files.host.push(("PATH", programs.into()));

        files
    }

    /// Returns the policy of a coding session sharing `directories` and
    /// `files` of the host's caches.
    fn policy(&self, directories: Vec<PathBuf>, files: Vec<PathBuf>) -> ExecutionPolicy {
        self.fixture.policy(Grants {
            read_scope: ReadScope::FullDisk,
            temporary_directory: Some(self.home.join("sessions/own")),
            ..host_cache_grants(directories, files)
        })
    }
}

/// Returns grants sharing the host cache `directories` and `files`, pinned
/// to what they resolve to now, following links as a pin recorded before a
/// link appeared would.
fn host_cache_grants(directories: Vec<PathBuf>, files: Vec<PathBuf>) -> Grants {
    let pin = |path: &PathBuf| {
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        let node = std::fs::metadata(&resolved)
            .map_or((0, 0), |metadata| (metadata.dev(), metadata.ino()));

        HostCachePin { node, resolved }
    };

    Grants {
        host_cache_pins: HostCachePins {
            directories: directories.iter().map(pin).collect(),
            files: files.iter().map(pin).collect(),
        },
        host_cache_directories: directories,
        host_cache_files: files,
        ..Grants::default()
    }
}

#[tokio::test]
async fn host_caches_resolve_beside_the_credentials_and_programs_they_exclude() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = &cache_home.home;
    let directories = vec![home.join(".cargo/registry"), home.join(".npm")];
    let lock = home.join(".cargo/.package-cache");

    // Act
    let shared = resolve_access(
        &cache_home.files(),
        &cache_home.policy(directories, vec![lock.clone()]),
    )
    .await
    .expect("shared caches");
    let narrow = resolve_access(
        &cache_home.files(),
        &cache_home
            .fixture
            .policy(host_cache_grants(vec![home.join("go/pkg/mod")], Vec::new())),
    )
    .await
    .expect("caches without full-disk reads");

    // Assert
    assert_eq!(
        shared.writes,
        [
            home.join("sessions/own"),
            home.join(".cargo/registry"),
            home.join(".npm")
        ]
    );
    assert_eq!(shared.write_nodes.len(), 3);
    assert_eq!(shared.write_files, [lock]);
    assert_eq!(shared.write_file_nodes.len(), 1);
    // Cargo's credentials beside the shared registry stay hidden.
    assert!(
        shared
            .hidden_paths
            .contains(&home.join(".cargo/credentials.toml"))
    );
    assert_eq!(narrow.writes, [home.join("go/pkg/mod")]);
}

#[tokio::test]
async fn host_caches_refuse_homes_credentials_programs_and_other_grants() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = cache_home.home.clone();
    let fixture = &cache_home.fixture;
    std::fs::hard_link(
        home.join(".cargo/credentials.toml"),
        home.join("credentials-link"),
    )
    .expect("hard link");
    std::fs::create_dir_all(home.join(".ssh/cache")).expect("credential store");
    std::fs::write(home.join(".cargo/bin/.package-cache"), "").expect("program file");
    symlink(home.join(".cargo"), home.join("cargo-alias")).expect("Cargo alias");
    symlink(&fixture.workspace, home.join("workspace-alias")).expect("workspace alias");
    symlink(&fixture.admin, home.join("metadata-alias")).expect("metadata alias");
    let directories = [
        // The home directory and its ancestors.
        home.clone(),
        fixture.outside.clone(),
        // Cargo's home holds its credentials and programs; a link, which
        // could be swapped later; a credential store and anything inside it.
        home.join(".cargo"),
        home.join("cargo-alias"),
        home.join(".ssh"),
        home.join(".ssh/cache"),
        // Program directories on `PATH`, and what contains them.
        home.join(".cargo/bin"),
        home.join("go"),
        // Git metadata, the workspace, and the other write grants.
        home.join("repository/.git/objects"),
        home.join("workspace-alias"),
        home.join("metadata-alias"),
        home.join("sessions/own"),
        home.join("sessions"),
        // Missing, not a directory, or a system directory.
        home.join("missing"),
        home.join(".cargo/.package-cache"),
        PathBuf::from("/usr/lib"),
    ];
    let files = [
        home.join(".cargo/credentials.toml"),
        home.join("credentials-link"),
        home.join(".cargo/registry"),
        home.join("missing.lock"),
        home.join(".cargo/bin/.package-cache"),
    ];

    // Act / Assert
    for directory in directories {
        assert_eq!(
            resolve_access(
                &cache_home.files(),
                &cache_home.policy(vec![directory.clone()], Vec::new())
            )
            .await
            .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{}",
            directory.display()
        );
    }
    for file in files {
        assert_eq!(
            resolve_access(
                &cache_home.files(),
                &cache_home.policy(Vec::new(), vec![file.clone()])
            )
            .await
            .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{}",
            file.display()
        );
    }
}

#[tokio::test]
async fn host_caches_refuse_a_homes_library_overlapping_caches_and_linked_files() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = &cache_home.home;
    let library_cache = home.join("Library/Caches/go-build");
    std::fs::create_dir_all(&library_cache).expect("Library cache");
    std::fs::create_dir_all(home.join(".cargo/registry/cache")).expect("nested cache");
    std::fs::write(home.join(".cargo/registry/.lock"), "").expect("lock inside a cache");
    symlink(
        home.join(".cargo/.package-cache"),
        home.join(".cargo/lock-alias"),
    )
    .expect("linked lock");
    let registry = home.join(".cargo/registry");
    let refused = [
        cache_home.policy(vec![library_cache], Vec::new()),
        cache_home.policy(vec![registry.clone(), registry.join("cache")], Vec::new()),
        cache_home.policy(vec![registry.join("cache"), registry.clone()], Vec::new()),
        cache_home.policy(vec![registry.clone()], vec![registry.join(".lock")]),
        cache_home.policy(Vec::new(), vec![home.join(".cargo/lock-alias")]),
    ];
    let accepted = cache_home.policy(
        vec![registry.clone(), home.join(".cargo/git")],
        vec![home.join(".cargo/.package-cache")],
    );

    // Act / Assert
    for policy in refused {
        assert_eq!(
            resolve_access(&cache_home.files(), &policy)
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{:?}",
            policy.host_cache_directories()
        );
    }
    let access = resolve_access(&cache_home.files(), &accepted)
        .await
        .expect("disjoint host caches");
    assert_eq!(access.write_files, [home.join(".cargo/.package-cache")]);
}

#[tokio::test]
async fn host_caches_refuse_credential_stores_without_full_disk_reads() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = &cache_home.home;
    std::fs::create_dir_all(home.join(".ssh/cache")).expect("credential store");
    // No program directory on `PATH`, so only the credentials conflict.
    let files = HomeInspection::at(home);
    let policy = |directory: PathBuf| {
        cache_home
            .fixture
            .policy(host_cache_grants(vec![directory], Vec::new()))
    };

    // Act / Assert
    for directory in [
        home.join(".cargo"),
        home.join(".ssh"),
        home.join(".ssh/cache"),
    ] {
        assert_eq!(
            resolve_access(&files, &policy(directory.clone()))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported),
            "{}",
            directory.display()
        );
    }
    let registry = resolve_access(&files, &policy(home.join(".cargo/registry")))
        .await
        .expect("the registry beside the credentials");
    assert_eq!(registry.writes, [home.join(".cargo/registry")]);
}

#[tokio::test]
async fn host_caches_refuse_commands_once_the_cache_was_replaced() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = &cache_home.home;
    let registry = home.join(".cargo/registry");
    let lock = home.join(".cargo/.package-cache");
    let directory_pinned = cache_home.policy(vec![registry.clone()], Vec::new());
    let file_pinned = cache_home.policy(Vec::new(), vec![lock.clone()]);
    let unpinned = cache_home.fixture.policy(Grants {
        host_cache_pins: HostCachePins::default(),
        ..host_cache_grants(vec![registry.clone()], vec![lock.clone()])
    });
    let elsewhere = home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("other directory");

    // Act
    let unchanged = resolve_access(&cache_home.files(), &directory_pinned).await;
    let without_pins = resolve_access(&cache_home.files(), &unpinned).await;
    std::fs::rename(&registry, home.join("moved")).expect("move the registry");
    symlink(&elsewhere, &registry).expect("link in its place");
    let linked = resolve_access(&cache_home.files(), &directory_pinned).await;
    std::fs::remove_file(&registry).expect("remove the link");
    std::fs::create_dir(&registry).expect("another directory in its place");
    let replaced = resolve_access(&cache_home.files(), &directory_pinned).await;
    // Kept under another name, so the replacement cannot reuse its inode.
    std::fs::rename(&lock, home.join("moved.lock")).expect("move the lock");
    std::fs::write(&lock, "").expect("another lock in its place");
    let replaced_lock = resolve_access(&cache_home.files(), &file_pinned).await;

    // Assert
    assert!(unchanged.is_ok());
    for refused in [without_pins, linked, replaced, replaced_lock] {
        assert_eq!(refused.map(|_| ()), Err(ExecutionError::Unsupported));
    }
}

#[tokio::test]
async fn host_caches_refuse_hidden_surroundings_sockets_and_unknown_homes() {
    // Arrange
    let cache_home = CacheHome::new();
    let home = cache_home.home.clone();
    let fixture = &cache_home.fixture;
    std::fs::create_dir_all(home.join(".npm/_cacache")).expect("npm content cache");
    // A hidden path around a cache, and a socket inside one.
    let around = fixture.policy(Grants {
        hidden_paths: vec![home.join(".npm")],
        ..host_cache_grants(vec![home.join(".npm/_cacache")], Vec::new())
    });
    let socket = fixture.policy(Grants {
        unix_sockets: vec![home.join(".npm/agent.sock")],
        ..host_cache_grants(vec![home.join(".npm")], Vec::new())
    });
    let lock_only = || {
        fixture.policy(host_cache_grants(
            Vec::new(),
            vec![home.join(".cargo/.package-cache")],
        ))
    };
    let unknown = HomeInspection {
        account: None,
        host: Vec::new(),
    };

    // Act / Assert
    for policy in [around, socket] {
        assert_eq!(
            resolve_access(&cache_home.files(), &policy)
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    // Without an account home nothing protects the home directories, and
    // inspection failing for a cache file grants nothing.
    let failing = FailingInspection::new("metadata", 2);
    for files in [&unknown as &dyn Inspection, &failing] {
        assert_eq!(
            resolve_access(files, &lock_only()).await.map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    assert!(
        resolve_access(&cache_home.files(), &lock_only())
            .await
            .is_ok()
    );
}

#[test]
fn host_caches_may_overlap_system_directories_only_inside_a_home() {
    // Arrange
    let homes = [PathBuf::from("/root")];
    let limits = HostCacheLimits {
        homes: &homes,
        program_directories: vec![PathBuf::from("/root/.cargo/bin")],
        protected: &[],
        taken: Vec::new(),
    };

    // Act / Assert
    assert!(limits.allows(Path::new("/root/.cargo/registry")));
    assert!(!limits.allows(Path::new("/root")));
    assert!(!limits.allows(Path::new("/usr/local/cache")));
    assert!(!limits.allows(Path::new("/root/.cargo/bin/cache")));
    assert!(!limits.allows(Path::new("/root/project/.GIT/objects")));
}
