//! Native sandbox coding-agent grants: full-disk reads with hidden paths,
//! writable roots outside the workspace, the Unix-socket allowlist, commands
//! excluded from the sandbox, and the denial flag a host escalates from.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ag_harness::bash::BashConfig;
use ag_harness::tool::ToolSet;
use ag_harness::{Harness, TurnOptions};
use serde_json::Value;
use tempfile::TempDir;

use super::fixture::{Workspace, schema, with_runtime};

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
    .with_environment("PATH".into(), "/usr/bin:/bin".into())
    .expect("PATH")
    .with_write("output".into())
    .expect("write grant");

    TurnOptions::from_output(schema())
        .with_tools(ToolSet::empty().with_bash())
        .with_bash(grant(with_runtime(configuration, &[])))
}

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
async fn native_full_disk_read_hides_denied_paths_and_writes_only_writable_roots() {
    // Arrange
    let workspace = Workspace::new();
    let outside = Outside::new();
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_read_denial(outside.path("secret"))
            .expect("hidden directory")
            .with_read_denial(outside.path("secret-file"))
            .expect("hidden file")
            .with_read_denial(outside.path("missing"))
            .expect("missing hidden path")
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
            "cat {} {} 2>/dev/null; ls {} 2>/dev/null",
            quoted(&outside.path("secret/key")),
            quoted(&outside.path("secret-file")),
            quoted(&outside.path("secret")),
        ),
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
            .is_some_and(|note| note.contains("sandbox blocked")),
        "{escaped}"
    );
    assert!(!outside.path("elsewhere/escaped").exists());
    let hidden_output = hidden["stdout"].as_str().expect("stdout");
    assert!(!hidden_output.contains("secret-value"), "{hidden}");
    assert!(!hidden_output.contains("key"), "{hidden}");
    workspace.assert_launcher_profiles();
}

#[tokio::test]
async fn native_excluded_commands_run_outside_the_sandbox() {
    // Arrange
    let workspace = Workspace::new();
    let outside = Outside::new();
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_excluded_command("touch".into())
            .expect("excluded command")
    });
    let harness = workspace.harness();

    // Act
    let excluded = run(
        &harness,
        &format!("touch {}", quoted(&outside.path("elsewhere/excluded"))),
        &options,
    )
    .await;
    let sandboxed = run(
        &harness,
        &format!(
            "command touch {}",
            quoted(&outside.path("elsewhere/sandboxed"))
        ),
        &options,
    )
    .await;

    // Assert
    assert_eq!(excluded["exit_code"], 0, "{excluded}");
    assert!(excluded.get("sandbox").is_none(), "{excluded}");
    assert!(outside.path("elsewhere/excluded").exists());
    assert_ne!(sandboxed["exit_code"], 0, "{sandboxed}");
    assert!(sandboxed["sandbox"].is_string(), "{sandboxed}");
    assert!(!outside.path("elsewhere/sandboxed").exists());
}

/// Seatbelt allows Unix-domain sockets by path; Linux keeps them denied.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_unix_socket_allowlist_allows_only_listed_sockets() {
    // Arrange
    let workspace = Workspace::new();
    // Socket paths are limited to about 100 bytes, so keep them short.
    let sockets = tempfile::tempdir_in("/tmp").expect("socket directory");
    let sockets = sockets.path().canonicalize().expect("canonical sockets");
    for name in ["listed", "unlisted"] {
        let listener = std::os::unix::net::UnixListener::bind(sockets.join(name)).expect("socket");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let _ = std::io::Write::write_all(&mut stream.expect("connection"), b"hello");
            }
        });
    }
    std::os::unix::fs::symlink(sockets.join("listed"), sockets.join("alias"))
        .expect("socket alias");
    let options = configuration(&workspace, |configuration| {
        configuration
            .with_unix_socket(sockets.join("alias"))
            .expect("socket grant")
    });
    let harness = workspace.harness();

    // Act
    let listed = run(
        &harness,
        &format!("nc -U {} </dev/null", quoted(&sockets.join("listed"))),
        &options,
    )
    .await;
    let unlisted = run(
        &harness,
        &format!("nc -U {} </dev/null", quoted(&sockets.join("unlisted"))),
        &options,
    )
    .await;

    // Assert
    assert_eq!(listed["exit_code"], 0, "{listed}");
    assert_eq!(listed["stdout"], "hello");
    assert_ne!(unlisted["exit_code"], 0, "{unlisted}");
    assert_eq!(unlisted["stdout"], "", "{unlisted}");
}
