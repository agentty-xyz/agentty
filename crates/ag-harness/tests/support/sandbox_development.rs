//! Native sandbox development grants on real repositories: Git in a linked
//! worktree with links, a per-session temporary directory, system and
//! toolchain-sized read grants without per-command inspection, an opt-in
//! network grant, and refused launches the model can recover from.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ag_harness::TurnOptions;
use ag_harness::bash::{BashConfig, CommandOutcome};
use ag_harness::tool::ToolSet;
use tempfile::TempDir;

use super::coverage::{self, Coverage};
use super::fixture::{refusing_harness, schema};

/// A repository whose linked worktree holds symlinks, beside a
/// per-session temporary directory, as a host would lay out one session.
struct LinkedWorktree {
    coverage: Option<Coverage>,
    directory: TempDir,
}

impl LinkedWorktree {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("repository fixture");
        let root = directory.path().canonicalize().expect("canonical fixture");
        let main = root.join("main");
        std::fs::create_dir(&main).expect("main worktree");
        std::fs::create_dir(root.join("session-tmp")).expect("temporary directory");
        git(&main, &["init", "-q", "-b", "main"]);
        std::fs::write(main.join("tracked"), "tracked-value").expect("tracked file");
        std::fs::create_dir(main.join("docs")).expect("linked directory");
        std::fs::write(main.join("docs/guide"), "guide-value").expect("guide");
        std::os::unix::fs::symlink("tracked", main.join("alias")).expect("file link");
        std::os::unix::fs::symlink("docs", main.join("docs-link")).expect("directory link");
        git(&main, &["add", "."]);
        git(&main, &["commit", "-q", "-m", "initial"]);
        git(
            &main,
            &["worktree", "add", "-q", "-b", "session", "../session"],
        );
        // Instrumented launchers write their profiles beneath `output`.
        std::fs::create_dir(root.join("session/output")).expect("profile parent");
        let coverage = Coverage::new(&root.join("session"));

        Self {
            coverage,
            directory,
        }
    }

    fn root(&self) -> PathBuf {
        self.directory
            .path()
            .canonicalize()
            .expect("canonical fixture")
    }

    fn worktree(&self) -> PathBuf {
        self.root().join("session")
    }

    fn temporary(&self) -> PathBuf {
        self.root().join("session-tmp")
    }

    /// Returns a configuration granting what a development session needs:
    /// the system directories, the whole worktree, its Git directories, and
    /// the temporary directory.
    fn configuration(&self) -> BashConfig {
        BashConfig::new(
            self.coverage
                .as_ref()
                .map_or_else(coverage::launcher, Coverage::launcher),
            "/bin/bash".into(),
            "development-v1".into(),
            Duration::from_secs(10),
            4096,
        )
        .expect("configuration")
        .with_host_information()
        .with_system_reads()
        .with_read(coverage::launcher())
        .expect("launcher grant")
        .with_write(".".into())
        .expect("worktree grant")
        .with_git_writes()
        .with_temporary_directory(self.temporary())
        .expect("temporary directory")
    }

    async fn run(&self, command: &str, configuration: BashConfig) -> CommandOutcome {
        let output = refusing_harness(&self.worktree())
            .turn(command, options(configuration))
            .await
            .expect("turn");

        serde_json::from_value(output.into_output()).expect("command result")
    }
}

fn options(configuration: BashConfig) -> TurnOptions {
    TurnOptions::from_output(schema())
        .with_tools(ToolSet::empty().with_bash())
        .with_bash(configuration)
}

/// Runs the host's Git outside the sandbox with a fixed identity.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args([
            "-c",
            "user.name=Host",
            "-c",
            "user.email=host@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("host Git");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).expect("Git output")
}

#[tokio::test]
async fn native_git_stages_and_commits_in_a_linked_worktree_with_links() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let common = fixture.root().join("main/.git");
    let configuration_before = std::fs::read(common.join("config")).expect("configuration");

    // Act
    let result = fixture
        .run(
            "set -e; git status --porcelain --untracked-files=no; /bin/cat alias docs-link/guide; \
             printf added > added; git add added; git -c user.name=Sandbox -c \
             user.email=sandbox@example.com -c commit.gpgsign=false commit -q -m sandboxed; \
             printf temporary > \"$TMPDIR/file\"; /bin/cat \"$TMPDIR/file\"; common=$(git \
             rev-parse --path-format=absolute --git-common-dir); if printf changed >> \
             \"$common/config\"; then exit 91; fi; if /bin/mkdir \"$common/hooks/escape\"; then \
             exit 92; fi; if printf changed > \"$(git rev-parse --absolute-git-dir)/commondir\"; \
             then exit 93; fi; if printf changed > .git; then exit 94; fi; printf confined",
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(
        result.stdout, "tracked-valueguide-valuetemporaryconfined",
        "{result:?}"
    );
    assert_eq!(
        git(&fixture.worktree(), &["log", "-1", "--format=%s"]).trim(),
        "sandboxed"
    );
    assert_eq!(git(&fixture.worktree(), &["show", "HEAD:added"]), "added");
    assert_eq!(
        std::fs::read(common.join("config")).expect("configuration"),
        configuration_before
    );
    assert!(!common.join("hooks/escape").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.temporary().join("file")).expect("temporary file"),
        "temporary"
    );
}

/// The system grants hold more than 100,000 entries (`/System` on macOS,
/// `/usr` on Linux), which the sandbox once walked and capped before every
/// command; it now grants them by path. Hard links between build outputs,
/// as Cargo leaves them in `target/`, and links in the workspace no longer
/// refuse the workspace either.
#[tokio::test]
async fn native_large_grants_hard_links_and_internal_links_need_no_inspection() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let deps = fixture.worktree().join("target/debug/deps");
    std::fs::create_dir_all(&deps).expect("build outputs");
    std::fs::write(deps.join("app-1234"), "built").expect("build output");
    std::fs::hard_link(deps.join("app-1234"), deps.join("../app")).expect("Cargo hard link");
    let configuration = fixture.configuration();
    let started = Instant::now();

    // Act
    let result = fixture
        .run(
            "set -e; /bin/cat target/debug/app; printf rebuilt > target/debug/deps/app-1234; \
             /bin/cat target/debug/app",
            configuration,
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "builtrebuilt", "{result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "granting large directories must not inspect them"
    );
}

/// Git run outside the sandbox resumes a rebase from its to-do list, which
/// can run commands, so a rebase the host left in progress stays read-only
/// and none can be started by writing new state.
#[tokio::test]
async fn native_git_writes_keep_rebase_state_read_only() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let admin = fixture.root().join("main/.git/worktrees/session");
    std::fs::create_dir(admin.join("rebase-merge")).expect("rebase in progress");
    std::fs::write(
        admin.join("rebase-merge/git-rebase-todo"),
        "pick 0000000 host\n",
    )
    .expect("to-do list");
    let todo = admin.join("rebase-merge/git-rebase-todo");

    // Act
    let result = fixture
        .run(
            &format!(
                "if printf 'exec escape\\n' >> '{}'; then exit 91; fi; if /bin/mkdir '{}'; then \
                 exit 92; fi; printf confined",
                todo.display(),
                admin.join("sequencer").display()
            ),
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "confined", "{result:?}");
    assert_eq!(
        std::fs::read_to_string(todo).expect("to-do list"),
        "pick 0000000 host\n"
    );
    assert!(!admin.join("sequencer").exists());
}

#[tokio::test]
async fn native_links_leaving_the_grants_cannot_be_followed() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let outside = fixture.root().join("outside");
    std::fs::create_dir(&outside).expect("outside directory");
    std::fs::write(outside.join("secret"), "secret").expect("secret");
    std::os::unix::fs::symlink(&outside, fixture.worktree().join("escape")).expect("escaping link");

    // Act
    let result = fixture
        .run(
            "if /bin/cat escape/secret; then exit 91; fi; if printf escaped > escape/written; \
             then exit 92; fi; printf confined",
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "confined", "{result:?}");
    assert!(!outside.join("written").exists());
}

#[tokio::test]
async fn native_network_grant_reaches_a_local_listener() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("address").port();
    let server = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().expect("sandboxed connection");
        let mut request = [0; 4];
        connection.read_exact(&mut request).expect("request");
        connection.write_all(b"pong").expect("response");

        request
    });

    // Act
    let result = fixture
        .run(
            &format!(
                "set -e; exec 3<>/dev/tcp/127.0.0.1/{port}; printf ping >&3; read -r -n 4 reply \
                 <&3; printf %s \"$reply\""
            ),
            fixture.configuration().with_network(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "pong", "{result:?}");
    assert_eq!(&server.join().expect("listener thread"), b"ping");
}

#[tokio::test]
async fn native_refused_launch_returns_a_correctable_tool_error() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let missing = fixture.root().join("missing-tmp");
    let configuration = BashConfig::new(
        coverage::launcher(),
        "/bin/bash".into(),
        "refused-v1".into(),
        Duration::from_secs(10),
        1024,
    )
    .expect("configuration")
    .with_host_information()
    .with_system_reads()
    .with_temporary_directory(missing.clone())
    .expect("temporary directory");

    // Act
    let output = refusing_harness(&fixture.worktree())
        .turn("printf escaped > escaped", options(configuration))
        .await
        .expect("a refused launch is answered, not fatal");

    // Assert
    assert_eq!(output.output()["status"], "rejected");
    assert!(
        output.output()["error"]
            .as_str()
            .expect("feedback")
            .starts_with("The sandbox refused to start this command"),
        "{:?}",
        output.output()
    );
    assert!(!fixture.worktree().join("escaped").exists());
    assert!(!missing.exists());
}
