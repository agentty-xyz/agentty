//! Native sandbox coding-agent grants: full-disk reads with hidden paths and
//! the built-in credential denials, writable roots outside the workspace,
//! the Unix-socket allowlist, approved commands excluded from the sandbox,
//! and the advisory denial note.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::approval::{ApprovalHandler, ApprovalRequest, Decision};
use ag_harness::bash::BashConfig;
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::sandbox::{IfUnavailable, Sandbox};
use ag_harness::tool::{ToolEffects, ToolSet};
use ag_harness::{Harness, Repository, TurnOptions};
use async_trait::async_trait;
use serde_json::Value;
use tempfile::TempDir;

use super::fixture::{ShellModel, Workspace, schema, with_runtime};
use crate::context_budget_fixture::unbounded_context_budget;

/// A workspace beside an outside tree holding a readable file, hidden
/// paths, a writable root, and a directory no grant covers.
struct Outside {
    directory: TempDir,
}

impl Outside {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("outside tree");
        let root = directory.path();
        std::fs::write(root.join("readable"), "readable-value").expect("readable file");
        std::fs::create_dir(root.join("secret")).expect("hidden directory");
        std::fs::write(root.join("secret/key"), "secret-value").expect("hidden key");
        std::fs::write(root.join("secret-file"), "secret-value").expect("hidden file");
        std::fs::create_dir(root.join("cache")).expect("writable root");
        std::fs::create_dir(root.join("elsewhere")).expect("ungranted directory");

        Self { directory }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory
            .path()
            .canonicalize()
            .expect("canonical outside tree")
            .join(name)
    }
}

/// Returns a configuration reading the whole disk, writing the workspace's
/// `output`, and granting what `grant` adds.
fn configuration(
    workspace: &Workspace,
    grant: impl FnOnce(BashConfig) -> BashConfig,
) -> TurnOptions {
    let configuration = BashConfig::new(
        workspace.launcher(),
        "/bin/bash".into(),
        "coding-v1".into(),
        Duration::from_secs(10),
        4096,
    )
    .expect("configuration")
    .with_host_information()
    .with_full_disk_read()
    .expect("full-disk read")
    .with_environment("PATH".into(), "/usr/bin:/bin".into())
    .expect("PATH")
    .with_write("output".into())
    .expect("write grant");

    TurnOptions::from_output(schema())
        .with_tools(ToolSet::empty().with_bash())
        .with_bash(grant(with_runtime(configuration, &[])))
}

/// Runs `command` as one turn and returns the `bash` result.
async fn run(harness: &Harness, command: &str, options: &TurnOptions) -> Value {
    harness
        .turn(command, options.clone())
        .await
        .expect("turn")
        .into_output()
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

#[tokio::test]
async fn native_full_disk_read_hides_hidden_paths_and_writes_only_writable_roots() {
    // Arrange
    let workspace = Workspace::new();
    let outside = Outside::new();
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_hidden_path(outside.path("secret"))
            .expect("hidden directory")
            .with_hidden_path(outside.path("secret-file"))
            .expect("hidden file")
            .with_hidden_path(outside.path("later"))
            .expect("hidden path missing at launch")
            .with_writable_root(outside.path("cache"))
            .expect("writable root")
    });
    let harness = workspace.harness();

    // Act
    let read = run(
        &harness,
        &format!("cat {}", quoted(&outside.path("readable"))),
        &options,
    )
    .await;
    let cached = run(
        &harness,
        &format!("printf cached > {}", quoted(&outside.path("cache/entry"))),
        &options,
    )
    .await;
    let escaped = run(
        &harness,
        &format!("printf x > {}", quoted(&outside.path("elsewhere/escaped"))),
        &options,
    )
    .await;
    let hidden = run(
        &harness,
        &format!(
            "cat {readable}; echo; cat {key}; echo \"key=$?\"; cat {file}; echo \"file=$?\"; ls \
             -A {secret}; echo \"list=$?\"; printf x > {new}; echo \"write=$?\"; grep -s -e null \
             -e {root} /proc/self/mountinfo >&2; true",
            root = quoted(&outside.path("")),
            readable = quoted(&outside.path("readable")),
            key = quoted(&outside.path("secret/key")),
            file = quoted(&outside.path("secret-file")),
            secret = quoted(&outside.path("secret")),
            new = quoted(&outside.path("secret/new")),
        ),
        &options,
    )
    .await;
    // A path hidden while missing stays hidden once it appears.
    std::fs::write(outside.path("later"), "secret-value").expect("later secret");
    let later = run(
        &harness,
        &format!("cat {}", quoted(&outside.path("later"))),
        &options,
    )
    .await;

    // Assert
    assert_eq!(read["exit_code"], 0, "{read}");
    assert_eq!(read["stdout"], "readable-value");
    assert_eq!(cached["exit_code"], 0, "{cached}");
    assert_eq!(
        std::fs::read_to_string(outside.path("cache/entry")).expect("cache entry"),
        "cached"
    );
    assert_ne!(escaped["exit_code"], 0, "{escaped}");
    assert!(
        escaped["sandbox"]
            .as_str()
            .is_some_and(|note| note.contains("If the sandbox caused it")),
        "{escaped}"
    );
    assert!(!outside.path("elsewhere/escaped").exists());
    // The control read works in the same command; the hidden directory
    // reads as missing on macOS and as empty on Linux, its key cannot be
    // read, nothing can be written into it, and the hidden file reads as
    // empty at most.
    let stdout = hidden["stdout"].as_str().expect("stdout");
    assert!(stdout.starts_with("readable-value\n"), "{hidden}");
    assert!(!stdout.contains("secret-value"), "{hidden}");
    assert!(stdout.contains("key=1"), "{hidden}");
    assert!(stdout.contains("write=1"), "{hidden}");
    if cfg!(target_os = "macos") {
        assert!(stdout.contains("file=1"), "{hidden}");
        assert!(stdout.contains("list=1"), "{hidden}");
    } else {
        assert!(stdout.contains("file=0"), "{hidden}");
        assert!(stdout.contains("list=0"), "{hidden}");
    }
    assert!(!outside.path("secret/new").exists());
    assert!(
        !later["stdout"]
            .as_str()
            .expect("stdout")
            .contains("secret-value"),
        "{later}"
    );
    workspace.assert_launcher_profiles();
}

/// The login keychain stays unreadable under full-disk reads, without any
/// hidden path the host names.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_full_disk_read_never_exposes_the_keychain() {
    // Arrange
    let workspace = Workspace::new();
    let keychains = std::env::home_dir()
        .expect("home directory")
        .join("Library/Keychains");
    let options = configuration(&workspace, BashConfig::with_network);

    // Act
    let listed = run(
        &workspace.harness(),
        &format!("ls {} >/dev/null", quoted(&keychains)),
        &options,
    )
    .await;

    // Assert
    assert_ne!(listed["exit_code"], 0, "{listed}");
    assert!(
        listed["stderr"]
            .as_str()
            .expect("stderr")
            .contains("Operation not permitted"),
        "{listed}"
    );
}

#[tokio::test]
async fn native_hidden_paths_inside_writable_directories_are_refused() {
    // Arrange
    let workspace = Workspace::new();
    std::fs::write(workspace.path().join("output/secret"), "secret-value").expect("secret");
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_hidden_path(workspace.path().join("output/secret"))
            .expect("hidden path")
    });

    // Act
    // Renaming `output` would otherwise reveal the hidden file.
    let result = workspace
        .harness()
        .turn("mv output moved && cat moved/secret", options)
        .await;

    // Assert
    assert!(result.is_err(), "{result:?}");
    assert!(workspace.path().join("output/secret").exists());
}

/// Approves every call and records the effects it was asked about.
#[derive(Default)]
struct Approver(Mutex<Vec<ToolEffects>>);

#[async_trait]
impl ApprovalHandler for Approver {
    async fn decide(&self, request: &ApprovalRequest) -> Decision {
        self.0.lock().expect("approvals").push(request.effects);

        Decision::Allow
    }
}

#[tokio::test]
async fn native_excluded_commands_run_outside_the_sandbox_once_approved() {
    // Arrange
    let workspace = Workspace::new();
    let outside = Outside::new();
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_excluded_command("/usr/bin/touch".into())
            .expect("excluded command")
    });
    let decisions = Arc::new(Approver::default());
    let with_approval = Harness::builder()
        .model(Arc::new(ShellModel))
        .context_budget(unbounded_context_budget())
        .repository(Repository::new(workspace.path(), "/usr/bin/git").expect("repository"))
        .execution_identity(ExecutionIdentity::new("native-shell", "1").expect("identity"))
        .sandbox(Sandbox::Native {
            if_unavailable: IfUnavailable::Refuse,
        })
        .approval(decisions.clone())
        .build()
        .expect("harness");
    let touch = |name: &str| format!("touch {}", quoted(&outside.path(name)));

    // Act
    let approved = run(&with_approval, &touch("elsewhere/approved"), &options).await;
    let unapproved = run(
        &workspace.harness(),
        &touch("elsewhere/unapproved"),
        &options,
    )
    .await;
    let compound = run(
        &with_approval,
        &format!("{}; true", touch("elsewhere/compound")),
        &options,
    )
    .await;

    // Assert
    assert_eq!(approved["exit_code"], 0, "{approved}");
    assert!(approved.get("sandbox").is_none(), "{approved}");
    assert!(outside.path("elsewhere/approved").exists());
    // Without an approval handler, or for a command that is not simple,
    // the command runs inside the sandbox.
    assert_ne!(unapproved["exit_code"], 0, "{unapproved}");
    assert!(unapproved["sandbox"].is_string(), "{unapproved}");
    assert!(!outside.path("elsewhere/unapproved").exists());
    assert!(!outside.path("elsewhere/compound").exists(), "{compound}");
    assert_eq!(
        *decisions.0.lock().expect("approvals"),
        [ToolEffects::ExecutesUnsandboxed, ToolEffects::Executes]
    );
}

/// Starts a listener at `path` that greets its one connection.
fn greeting_socket(path: &Path) {
    let listener = std::os::unix::net::UnixListener::bind(path).expect("socket");
    std::thread::spawn(move || {
        if let Some(Ok(mut stream)) = listener.incoming().next() {
            let _ = std::io::Write::write_all(&mut stream, b"hello");
        }
    });
}

/// Seatbelt allows Unix-domain sockets by path.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_unix_socket_allowlist_allows_only_listed_sockets() {
    // Arrange
    let workspace = Workspace::new();
    // Socket paths are limited to about 100 bytes, so keep them short.
    let sockets = tempfile::tempdir_in("/tmp").expect("socket directory");
    let sockets = sockets.path().canonicalize().expect("canonical sockets");
    for name in ["listed", "unlisted"] {
        greeting_socket(&sockets.join(name));
    }
    std::os::unix::fs::symlink(sockets.join("listed"), sockets.join("alias"))
        .expect("socket alias");
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_unix_socket(sockets.join("alias"))
            .expect("socket grant")
    });
    let harness = workspace.harness();
    // `-d` keeps `nc` from reading standard input, so it reads the greeting.
    let connect = |name: &str| format!("nc -d -U {}", quoted(&sockets.join(name)));

    // Act
    let listed = run(&harness, &connect("listed"), &options).await;
    let unlisted = run(&harness, &connect("unlisted"), &options).await;

    // Assert
    assert_eq!(listed["exit_code"], 0, "{listed}");
    assert_eq!(listed["stdout"], "hello");
    assert_ne!(unlisted["exit_code"], 0, "{unlisted}");
    assert_eq!(unlisted["stdout"], "", "{unlisted}");
}

/// Linux cannot allow sockets by path, so a socket grant warns, or, as
/// here, refuses the turn when the harness refuses to run without it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn native_unix_socket_grants_are_refused_on_linux() {
    // Arrange
    let workspace = Workspace::new();
    let sockets = tempfile::tempdir_in("/tmp").expect("socket directory");
    let socket = sockets
        .path()
        .canonicalize()
        .expect("canonical")
        .join("agent");
    greeting_socket(&socket);
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_unix_socket(socket.clone())
            .expect("socket grant")
    });

    // Act
    let result = workspace.harness().turn("true", options).await;

    // Assert
    assert!(
        matches!(
            result,
            Err(ag_harness::TurnError::SandboxUnavailable(
                ag_harness::sandbox::SandboxUnavailableReason::UnixSocketsUnsupported
            ))
        ),
        "{result:?}"
    );
}
