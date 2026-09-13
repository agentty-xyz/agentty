use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command as HostCommand, ExitStatus};
use std::time::{Duration, Instant};

use tempfile::TempDir;

use crate::execution::contract::{Command, Grants, Policy};
use crate::execution::macos::configuration::Configuration;
use crate::execution::macos::launch::{
    Child, c_arguments, check, retry_interrupted, validate_runtime, wait,
};

struct Fixture {
    executable: PathBuf,
    root: TempDir,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("create owned fixture root");
        let path = root.path().canonicalize().expect("canonical fixture path");
        let workspace = path.join("workspace");
        fs::create_dir(&workspace).expect("native isolation fixture operation succeeds");
        fs::create_dir(workspace.join(".git"))
            .expect("native isolation fixture operation succeeds");
        fs::write(workspace.join(".git/config"), "metadata")
            .expect("native isolation fixture operation succeeds");
        fs::write(workspace.join("readable"), "workspace")
            .expect("native isolation fixture operation succeeds");
        fs::create_dir(workspace.join("writable"))
            .expect("native isolation fixture operation succeeds");
        fs::write(path.join("secret"), "secret")
            .expect("native isolation fixture operation succeeds");
        let executable = path.join("probe");
        let source = path.join("probe.c");
        fs::write(&source, include_str!("probe_test.c"))
            .expect("native isolation fixture operation succeeds");
        let result = HostCommand::new("/usr/bin/xcrun")
            .args(["clang", "-Wall", "-Wextra", "-Werror"])
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("native isolation fixture operation succeeds");
        assert!(
            result.status.success(),
            "native probe requires Xcode Command Line Tools: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        Self {
            executable,
            root,
            workspace,
        }
    }

    fn configuration(&self) -> Configuration {
        let scratch = TempDir::new_in(
            self.root
                .path()
                .canonicalize()
                .expect("canonical fixture path"),
        )
        .expect("native isolation fixture operation succeeds");
        let policy = Policy::new(
            self.workspace.clone(),
            vec![self.workspace.join(".git")],
            Grants {
                environment: vec![("EXPLICIT".into(), "literal $value; *".into())],
                external_entries: vec!["/".into(), "/usr/bin/env".into(), self.executable.clone()],
                host_information: true,
                workspace_writes: vec!["writable".into()],
                ..Grants::default()
            },
        )
        .expect("native isolation fixture operation succeeds");

        Configuration::new(policy, scratch).expect("native isolation fixture operation succeeds")
    }

    fn command(&self, operation: &str, arguments: &[OsString]) -> Command {
        let mut argv = vec![operation.into()];
        argv.extend_from_slice(arguments);

        Command::new(self.executable.clone(), argv, ".".into())
            .expect("native isolation fixture operation succeeds")
    }

    fn isolated(&self, operation: &str, arguments: &[OsString]) -> ExitStatus {
        let configuration = self.configuration();
        let command = self.command(operation, arguments);
        let child = Child::spawn(configuration, &command)
            .expect("native isolation fixture operation succeeds");

        finish(child)
    }

    fn control(&self, operation: &str, arguments: &[OsString]) -> ExitStatus {
        HostCommand::new(&self.executable)
            .arg(operation)
            .args(arguments)
            .current_dir(&self.workspace)
            .output()
            .expect("native isolation fixture operation succeeds")
            .status
    }

    fn path(&self, path: &str) -> OsString {
        self.workspace.join(path).into_os_string()
    }
}

fn finish(mut child: Child) -> ExitStatus {
    let scratch = child.scratch().to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("query owned child status") {
            break status;
        }
        assert!(Instant::now() < deadline, "isolated child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        child.try_wait().expect("query owned child status"),
        Some(status)
    );
    let mut output = Vec::new();
    child
        .stdout
        .read_to_end(&mut output)
        .expect("native isolation fixture operation succeeds");
    child
        .stderr
        .read_to_end(&mut output)
        .expect("native isolation fixture operation succeeds");
    assert!(
        status.code().is_some(),
        "probe failed during runtime startup: {status}: {}",
        String::from_utf8_lossy(&output)
    );
    child.cleanup().expect("report successful scratch cleanup");
    drop(child);
    assert!(!scratch.exists());

    status
}

#[test]
fn native_filesystem_and_git_policy_has_positive_controls() {
    // Arrange
    let fixture = Fixture::new();
    let secret = fixture
        .root
        .path()
        .canonicalize()
        .expect("native isolation fixture operation succeeds")
        .join("secret")
        .into_os_string();

    // Act / Assert
    for (operation, arguments, allowed) in [
        ("read", vec![fixture.path("readable")], true),
        ("read", vec![fixture.path(".git/config")], true),
        ("read", vec![secret.clone()], false),
        ("exec-read", vec![secret], false),
        ("write", vec![fixture.path("readable")], false),
        ("write", vec![fixture.path(".git/config")], false),
        ("write", vec![fixture.path("writable/new")], true),
        ("mkdir", vec![fixture.path("writable/new-dir")], true),
    ] {
        assert!(
            fixture.control(operation, &arguments).success(),
            "positive control: {operation}"
        );
        if operation == "mkdir" {
            fs::remove_dir(Path::new(&arguments[0]))
                .expect("native isolation fixture operation succeeds");
        }
        assert_eq!(
            fixture.isolated(operation, &arguments).success(),
            allowed,
            "isolated {operation}: {arguments:?}"
        );
    }
}

#[test]
fn native_link_special_node_and_directory_rename_denials_have_controls() {
    // Arrange
    let fixture = Fixture::new();

    // Act / Assert
    for operation in ["link", "symlink", "fifo", "rename"] {
        let destination = fixture.path(&format!("writable/{operation}"));
        let arguments = match operation {
            "fifo" => vec![destination.clone()],
            "rename" => {
                fs::create_dir(fixture.workspace.join("writable/source"))
                    .expect("native isolation fixture operation succeeds");
                vec![fixture.path("writable/source"), destination.clone()]
            }
            _ => vec![fixture.path("readable"), destination.clone()],
        };
        assert!(
            !fixture.isolated(operation, &arguments).success(),
            "isolated {operation}"
        );
        assert!(
            fixture.control(operation, &arguments).success(),
            "positive control: {operation}"
        );
        if operation == "rename" {
            fs::remove_dir(Path::new(&destination))
                .expect("native isolation fixture operation succeeds");
        } else {
            fs::remove_file(Path::new(&destination))
                .expect("native isolation fixture operation succeeds");
        }
    }
}

#[test]
fn native_network_environment_descriptors_and_fork_policy_has_controls() {
    // Arrange
    let fixture = Fixture::new();
    let network =
        TcpListener::bind("127.0.0.1:0").expect("native isolation fixture operation succeeds");
    let socket_path = fixture.root.path().join("socket");
    let socket =
        UnixListener::bind(&socket_path).expect("native isolation fixture operation succeeds");
    let descriptor = fs::File::open(fixture.workspace.join("readable"))
        .expect("native isolation fixture operation succeeds");
    rustix::io::fcntl_setfd(&descriptor, rustix::io::FdFlags::empty())
        .expect("native isolation fixture operation succeeds");
    let inherited_path = std::env::var_os("PATH").expect("test runner has PATH");

    // Act / Assert
    for (operation, arguments, allowed) in [
        (
            "network",
            vec![
                network
                    .local_addr()
                    .expect("native isolation fixture operation succeeds")
                    .port()
                    .to_string()
                    .into(),
            ],
            false,
        ),
        ("unix", vec![socket_path.into_os_string()], false),
        ("fd", vec![descriptor.as_raw_fd().to_string().into()], false),
        ("environment", vec!["PATH".into(), inherited_path], false),
        ("host", vec![], true),
        ("fork", vec![], false),
        ("spawn", vec![], false),
    ] {
        assert!(
            fixture.control(operation, &arguments).success(),
            "positive control: {operation}"
        );
        assert_eq!(
            fixture.isolated(operation, &arguments).success(),
            allowed,
            "isolated {operation}"
        );
    }
    assert!(
        fixture
            .isolated(
                "environment",
                &["EXPLICIT".into(), "literal $value; *".into()]
            )
            .success()
    );
    drop(socket);
}

#[test]
fn native_drop_kills_reaps_and_releases_scratch() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture.configuration();
    let command = fixture.command("sleep", &[]);
    let mut child =
        Child::spawn(configuration, &command).expect("native isolation fixture operation succeeds");
    let scratch = child.scratch().to_owned();

    // Act
    assert!(
        child
            .try_wait()
            .expect("query owned child status")
            .is_none()
    );
    assert_eq!(
        child
            .cleanup()
            .expect_err("cannot clean a live process")
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(child);

    // Assert
    assert!(!scratch.exists());
}

#[test]
fn native_setup_errors_release_resources() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture.configuration();
    let scratch = configuration.scratch().to_owned();
    let command = Command::new("/usr/bin/true".into(), vec![], ".".into())
        .expect("native isolation fixture operation succeeds");

    // Act
    let result = Child::spawn(configuration, &command);

    // Assert
    assert!(result.is_err());
    assert!(!scratch.exists());
    assert!(check(libc::EINVAL).is_err());
    assert!(wait(i32::MAX, libc::WNOHANG).is_err());
}

#[test]
fn native_scratch_and_linked_git_administration_are_scoped() {
    // Arrange
    let fixture = Fixture::new();
    fs::remove_dir_all(fixture.workspace.join(".git"))
        .expect("native isolation fixture operation succeeds");
    fs::create_dir(fixture.workspace.join("writable/admin"))
        .expect("native isolation fixture operation succeeds");
    fs::create_dir(fixture.workspace.join("writable/common"))
        .expect("native isolation fixture operation succeeds");
    fs::write(
        fixture.workspace.join("writable/admin/commondir"),
        "../common\n",
    )
    .expect("native isolation fixture operation succeeds");
    fs::write(fixture.workspace.join(".git"), "gitdir: writable/admin\n")
        .expect("native isolation fixture operation succeeds");
    let configuration = fixture.configuration();
    let scratch_file = configuration.scratch().join("new").into_os_string();
    let command = fixture.command("write", &[scratch_file]);

    // Act / Assert
    assert!(
        finish(
            Child::spawn(configuration, &command)
                .expect("native isolation fixture operation succeeds")
        )
        .success()
    );
    for name in [
        "writable/admin/config",
        "writable/common/config",
        "writable/.GiT",
    ] {
        let arguments = [fixture.path(name)];
        assert!(!fixture.isolated("write", &arguments).success(), "{name}");
        assert!(
            fixture.control("write", &arguments).success(),
            "positive control: {name}"
        );
        fs::remove_file(fixture.workspace.join(name))
            .expect("native isolation fixture operation succeeds");
    }
}

#[test]
fn native_identity_is_visible_without_sysctl_authorization() {
    // Arrange
    let fixture = Fixture::new();
    let profile = format!(
        "(version 1)(deny default)(allow process-exec)(allow file-read* (literal \"/\") (literal \
         \"{}\"))",
        fixture.executable.display()
    );

    // Act / Assert
    for (operation, allowed) in [("uid", true), ("host", false)] {
        assert!(fixture.control(operation, &[]).success());
        let output = HostCommand::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile])
            .arg(&fixture.executable)
            .arg(operation)
            .env_clear()
            .output()
            .expect("native isolation fixture operation succeeds");
        assert!(
            output.status.code().is_some(),
            "runtime must start for the probe"
        );
        assert_eq!(output.status.success(), allowed, "{operation}");
    }
}

#[test]
fn runtime_support_does_not_extend_to_other_platforms() {
    // Arrange / Act / Assert
    assert!(validate_runtime("aarch64", b"26.6.2\n", true).is_ok());
    for (architecture, version, success) in [
        ("x86_64", b"26.6.2".as_slice(), true),
        ("aarch64", b"15.0".as_slice(), true),
        ("aarch64", b"26.6.2".as_slice(), false),
    ] {
        assert!(validate_runtime(architecture, version, success).is_err());
    }
}

#[test]
fn native_workspace_root_write_grant_preserves_git_metadata() {
    // Arrange
    let fixture = Fixture::new();
    let scratch = TempDir::new_in(
        fixture
            .root
            .path()
            .canonicalize()
            .expect("canonical fixture root"),
    )
    .expect("owned scratch");
    let policy = Policy::new(
        fixture.workspace.clone(),
        vec![fixture.workspace.join(".git")],
        Grants {
            host_information: true,
            external_entries: vec![
                "/".into(),
                "/usr/bin/env".into(),
                fixture.executable.clone(),
            ],
            workspace_writes: vec![".".into()],
            ..Grants::default()
        },
    )
    .expect("root write policy");
    let configuration = Configuration::new(policy, scratch).expect("validated root write policy");
    let command = fixture.command("write", &[fixture.path(".git/config")]);

    // Act
    let status =
        finish(Child::spawn(configuration, &command).expect("launch with root write policy"));

    // Assert
    assert!(!status.success());
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(".git/config")).expect("read metadata"),
        "metadata"
    );
}

#[test]
fn native_argv_encoding_rejects_nul_before_spawn() {
    // Arrange
    let arguments = vec![OsString::from("invalid\0argument")];

    // Act / Assert
    assert!(c_arguments(&arguments).is_err());
}

#[test]
fn interrupted_reaping_retries_without_losing_the_result() {
    // Arrange
    let mut calls = 0;

    // Act
    let result = retry_interrupted(|| {
        calls += 1;
        if calls == 1 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }

        Ok(42)
    });

    // Assert
    assert_eq!(result.expect("retry succeeds"), 42);
    assert_eq!(calls, 2);
}

#[test]
fn native_executable_assignments_cannot_promote_an_argument_to_a_command() {
    // Arrange
    let mut fixture = Fixture::new();
    let alternate = fixture.executable.clone();
    fixture.executable = fixture.workspace.join("tool=x");
    fs::copy(&alternate, &fixture.executable).expect("copy valid executable with equals in name");
    let marker = fixture.path("writable/argument-executed");
    let command = Command::new(
        fixture.executable.clone(),
        vec![alternate.into_os_string(), "write".into(), marker.clone()],
        ".".into(),
    )
    .expect("portable command contract accepts equals");
    let configuration = fixture.configuration();
    let scratch = configuration.scratch().to_owned();

    // Act
    let result = Child::spawn(configuration, &command);

    // Assert
    let error = result
        .err()
        .expect("reject env assignment ambiguity before launch");
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("containing '='"));
    assert!(!Path::new(&marker).exists());
    assert!(!scratch.exists());
    assert!(
        fixture.control("write", &[marker]).success(),
        "direct execution positive control"
    );
}

#[test]
fn native_scratch_metadata_mutations_are_denied_with_positive_controls() {
    // Arrange
    let fixture = Fixture::new();

    // Act / Assert
    for operation in ["chmod", "chflags"] {
        let configuration = fixture.configuration();
        let path = configuration.scratch().join("directory");
        fs::create_dir(&path).expect("create scratch directory");
        let arguments = [path.clone().into_os_string()];
        let command = fixture.command(operation, &arguments);
        let status = finish(Child::spawn(configuration, &command).expect("launch mutation probe"));
        assert!(!status.success(), "isolated {operation} must be denied");
        assert!(
            !path.exists(),
            "scratch is removed despite hostile mutation attempt"
        );
        let control = fixture.workspace.join("writable/control");
        fs::create_dir(&control).expect("create control directory");
        let control_arguments = [control.clone().into_os_string()];
        let status = fixture.control(operation, &control_arguments);
        let restored = fixture.control("restore-metadata", &control_arguments);
        assert!(
            restored.success(),
            "restore control flags and permissions before assertions"
        );
        assert!(status.success(), "unsandboxed {operation} positive control");
        fs::remove_dir(control).expect("remove control directory");
    }
}

#[test]
fn native_restrictive_directory_creation_is_cleaned_after_reaping() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture.configuration();
    let path = configuration.scratch().join("no-access");
    let command = fixture.command("mkdir-locked", &[path.clone().into_os_string()]);

    // Act
    let status = finish(Child::spawn(configuration, &command).expect("launch restrictive mkdir"));

    // Assert
    assert!(status.success(), "creation mode itself is permitted");
    assert!(
        !path.exists(),
        "cleanup restores access before recursive removal"
    );
}

#[test]
fn native_scratch_cleanup_handles_trees_beyond_path_max() {
    // Arrange
    let fixture = Fixture::new();
    let configuration = fixture.configuration();
    let scratch = configuration.scratch().to_owned();
    let command = fixture.command("deep-tree", &[scratch.clone().into_os_string()]);

    // Act
    let status = finish(Child::spawn(configuration, &command).expect("launch deep mkdir probe"));

    // Assert
    assert!(
        status.success(),
        "create a hierarchy beyond PATH_MAX under isolation"
    );
    assert!(!scratch.exists(), "remove the entire deep scratch tree");
    let control = fixture.configuration();
    let control_path = control.scratch().to_owned();
    let status = fixture.control("deep-tree", &[control_path.clone().into_os_string()]);
    control.cleanup().expect("clean deep positive-control tree");
    assert!(status.success(), "unsandboxed deep mkdir positive control");
    assert!(!control_path.exists());
}

#[test]
fn native_non_git_alternates_suffix_remains_readable_and_writable() {
    // Arrange
    let fixture = Fixture::new();
    let path = "writable/fixtures/objects/info/alternates";
    fs::create_dir_all(fixture.workspace.join("writable/fixtures/objects/info"))
        .expect("create ordinary fixture directory");
    fs::write(fixture.workspace.join(path), "fixture contents").expect("write ordinary fixture");

    // Act
    let read = fixture.isolated("read", &[fixture.path(path)]);
    let write = fixture.isolated("write", &[fixture.path(path)]);

    // Assert
    assert!(read.success(), "read ordinary fixture under isolation");
    assert!(
        write.success(),
        "write ordinary fixture under its explicit grant"
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(path)).expect("read updated fixture"),
        "changed"
    );
}

#[test]
fn native_launch_revalidates_external_git_aliases_before_writing() {
    // Arrange
    let fixture = Fixture::new();
    let admin = fixture
        .root
        .path()
        .canonicalize()
        .expect("canonical root")
        .join("admin");
    fs::create_dir(&admin).expect("create external Git directory");
    fs::remove_dir_all(fixture.workspace.join(".git")).expect("remove Git directory");
    fs::write(
        fixture.workspace.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("write linked worktree pointer");
    let target = fixture.workspace.join("writable/config");
    assert!(
        fixture
            .isolated("write", &[target.clone().into_os_string()])
            .success()
    );
    let configuration = fixture.configuration();
    let scratch = configuration.scratch().to_owned();
    let alias = admin.join("config");
    symlink(&target, &alias).expect("simulate host adding a metadata alias after validation");
    assert!(
        fixture
            .control("write", &[alias.into_os_string()])
            .success()
    );
    fs::write(&target, "metadata").expect("reset target after positive control");
    let command = fixture.command("write", &[target.clone().into_os_string()]);

    // Act
    let error = Child::spawn(configuration, &command)
        .err()
        .expect("reject alias before spawn");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert!(error.to_string().contains("noncanonical"));
    assert_eq!(
        fs::read_to_string(target).expect("metadata remains intact"),
        "metadata"
    );
    assert!(!scratch.exists());
}
