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
    Access, GitDirectories, Host, Inspection, LocalInspection, Native, NativeLaunch, NativeProcess,
    bounded_bytes, cache_root, git_directories, pointer, probe, resolve_access, resolve_hidden,
    validate_executable,
};
use crate::bash::BashConfig;
use crate::command_journal::CommandCleanupScope;
use crate::execution::contract::{
    BashExecutor, BashProcess, Enforcement, ExecutionCommand, ExecutionError, ExecutionPolicy,
    Grants, MainExit, ProcessEvent, ReadScope,
};
use crate::execution::wire::{GitWrites, Notice};
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

#[tokio::test]
async fn git_writes_refuse_per_worktree_configuration_the_worktree_lacks() {
    // Arrange
    let directory = tempfile::tempdir().expect("directory");
    let root = directory.path().canonicalize().expect("canonical");
    let common = root.join("common");
    let admin = common.join("worktrees/session");
    std::fs::create_dir_all(&admin).expect("worktree administration");
    let linked = || GitDirectories {
        admin: admin.clone(),
        common: common.clone(),
    };

    // Act / Assert
    assert_eq!(
        GitDirectories {
            admin: common.clone(),
            common: common.clone(),
        }
        .writes(&LocalInspection)
        .await,
        Ok(GitWrites {
            admin: None,
            common: common.clone(),
        })
    );
    assert_eq!(
        linked().writes(&LocalInspection).await,
        Err(ExecutionError::Setup),
        "a repository without configuration"
    );
    std::fs::write(common.join("config"), "[core]\n\tbare = false\n").expect("configuration");
    let expected = Ok(GitWrites {
        admin: Some(admin.clone()),
        common: common.clone(),
    });
    assert_eq!(linked().writes(&LocalInspection).await, expected);
    std::fs::write(
        common.join("config"),
        "[extensions]\n\tworktreeConfig = true\n",
    )
    .expect("per-worktree configuration");
    assert_eq!(
        linked().writes(&LocalInspection).await,
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        linked().writes(&FailingInspection::new("exists", 1)).await,
        Err(ExecutionError::Setup)
    );
    std::fs::write(admin.join("config.worktree"), "").expect("worktree configuration");
    assert_eq!(linked().writes(&LocalInspection).await, expected);
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
        std::fs::write(common.join("config"), "").expect("configuration");

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
async fn access_resolves_grants_by_path_and_opens_the_repository_git_directories() {
    // Arrange
    let fixture = AccessFixture::new();
    symlink(&fixture.outside, fixture.temporary.join("alias")).expect("read alias");
    let policy = fixture.policy(Grants {
        external_reads: vec![fixture.outside.clone(), fixture.temporary.join("alias")],
        git_writes: true,
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
    assert_eq!(
        access.git_writes,
        Some(GitWrites {
            admin: Some(fixture.admin.clone()),
            common: fixture.common.clone(),
        })
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
    let unrelated = tempfile::tempdir().expect("repository-free workspace");
    let unrelated = unrelated.path().canonicalize().expect("canonical");
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
    let without_repository = ExecutionPolicy::new(
        unrelated.clone(),
        vec![unrelated.join(".git")],
        Grants {
            git_writes: true,
            ..Grants::default()
        },
    )
    .expect("policy");
    assert_eq!(
        resolve_access(&LocalInspection, &without_repository)
            .await
            .map(|_| ()),
        Err(ExecutionError::Unsupported)
    );
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
}

/// Local inspection with another home directory.
struct HomeInspection(PathBuf);

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
        Some(self.0.clone())
    }
}

#[tokio::test]
async fn access_resolves_writable_roots_hidden_paths_and_sockets() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let registry = home.join(".cargo/registry");
    let sessions = fixture.outside.join("sessions");
    let secret = fixture.outside.join("secret");
    for directory in [&registry, &sessions.join("own"), &sessions.join("other")] {
        std::fs::create_dir_all(directory).expect("fixture directory");
    }
    std::fs::write(&secret, "secret").expect("secret");
    symlink(&secret, fixture.outside.join("secret-alias")).expect("secret alias");
    let socket = fixture.outside.join("agent.sock");
    let policy = fixture.policy(Grants {
        hidden_paths: vec![
            fixture.outside.join("secret-alias"),
            fixture.outside.join("missing/key"),
        ],
        read_scope: ReadScope::FullDisk,
        temporary_directory: Some(sessions.join("own")),
        unix_sockets: vec![socket.clone()],
        writable_roots: vec![registry.clone()],
        ..Grants::default()
    });

    // Act
    let access = resolve_access(&HomeInspection(home.clone()), &policy)
        .await
        .expect("access");

    // Assert
    assert_eq!(access.writes, [sessions.join("own"), registry]);
    assert_eq!(access.write_nodes.len(), 2);
    // Links resolve, and a missing path keeps its name beneath its nearest
    // existing ancestor; full-disk reads add the credential stores and the
    // other sessions' directories.
    let mut expected = vec![secret, fixture.outside.join("missing/key")];
    expected.extend(
        [
            "Library/Keychains",
            ".ssh",
            ".aws",
            ".gnupg",
            ".config/gh/hosts.yml",
            ".netrc",
            ".docker/config.json",
            ".config/gcloud",
            ".kube/config",
        ]
        .map(|path| home.join(path)),
    );
    expected.push(resolve_hidden(&LocalInspection, Path::new("/run/secrets")).await);
    expected.push(sessions);
    assert_eq!(access.hidden_paths, expected);
    assert_eq!(access.unix_sockets, [socket]);
}

#[tokio::test]
async fn full_disk_reads_keep_shared_temporary_parents_visible() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    std::fs::create_dir_all(home.join("session")).expect("home session");
    let system = std::env::temp_dir()
        .canonicalize()
        .expect("system temporary");
    let own = tempfile::tempdir_in(&system).expect("system session");

    // Act / Assert
    // Directly in the system temporary directory, in the home directory,
    // or beside the workspace, the parent's other entries stay visible.
    for temporary in [
        own.path().to_path_buf(),
        home.join("session"),
        fixture.temporary.clone(),
    ] {
        let policy = fixture.policy(Grants {
            read_scope: ReadScope::FullDisk,
            temporary_directory: Some(temporary.clone()),
            ..Grants::default()
        });
        let access = resolve_access(&HomeInspection(home.clone()), &policy)
            .await
            .expect("access");
        let parent = temporary
            .canonicalize()
            .expect("canonical temporary")
            .parent()
            .expect("parent")
            .to_path_buf();
        assert!(!access.hidden_paths.contains(&parent), "{temporary:?}");
    }
}

#[tokio::test]
async fn access_refuses_coding_grants_the_sandbox_cannot_honor() {
    // Arrange
    let fixture = AccessFixture::new();
    let home = fixture.outside.join("home");
    let tool_cache = home.join(".cache/tool");
    for directory in [&tool_cache, &home.join(".cargo"), &home.join(".cache/prek")] {
        std::fs::create_dir_all(directory).expect("home directory");
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
        // Writable roots that host tools execute from, or that are not
        // cache locations.
        Grants {
            writable_roots: vec![home.clone()],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![fixture.outside.clone()],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".cargo")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".cache")],
            ..Grants::default()
        },
        Grants {
            writable_roots: vec![home.join(".cache/prek")],
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
            resolve_access(&HomeInspection(home.clone()), &fixture.policy(grants))
                .await
                .map(|_| ()),
            Err(ExecutionError::Unsupported)
        );
    }
    let policy = fixture.policy(in_cache(Grants::default()));
    assert!(
        resolve_access(&HomeInspection(home.clone()), &policy)
            .await
            .is_ok()
    );
}

#[test]
fn writable_roots_must_be_caches_host_tools_do_not_execute() {
    // Arrange
    let home = Path::new("/home/user");
    let cases = [
        ("/home/user/.cargo/registry", true),
        ("/home/user/.cargo/git/db", true),
        ("/home/user/.npm/_cacache", true),
        ("/home/user/go/pkg/mod", true),
        ("/home/user/.cache/sccache", true),
        ("/home/user/Library/Caches/Homebrew", true),
        ("/private/var/folders/x/T/session-cache", true),
        ("/home/user", false),
        ("/home/user/.cargo", false),
        ("/home/user/.cargo/bin", false),
        ("/home/user/go", false),
        ("/home/user/go/bin", false),
        ("/home/user/.config", false),
        ("/home/user/.local/bin", false),
        ("/home/user/.npm", false),
        ("/home/user/.cache", false),
        ("/home/user/.cache/prek", false),
        ("/home/user/.cache/pre-commit/repo", false),
        ("/home/user/Library/Caches/ms-playwright", false),
        ("/home", false),
        ("/", false),
        ("/opt/homebrew", false),
        ("/usr/local", false),
        ("/etc", false),
        ("/Library/Caches", false),
        ("/var", false),
    ];

    // Act / Assert
    for (root, expected) in cases {
        assert_eq!(cache_root(Path::new(root), Some(home)), expected, "{root}");
    }
    // Without a known home directory, only system directories are refused.
    assert!(cache_root(Path::new("/home/user/.cargo"), None));
    assert!(!cache_root(Path::new("/usr/local"), None));
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
    let git = failing.clone().with_git_writes();
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
            &git,
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
