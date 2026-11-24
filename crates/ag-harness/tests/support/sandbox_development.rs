//! Native sandbox development grants on real repositories: Git in a linked
//! worktree with links, a per-session temporary directory, system and
//! toolchain-sized read grants without per-command inspection, an opt-in
//! network grant, refused launches the model can recover from, hard links
//! that cannot reach outside the grants, a real toolchain from outside the
//! system directories, and a bound on per-command latency.

use std::io::{Read, Write};
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ag_harness::TurnOptions;
use ag_harness::bash::{BashConfig, CommandOutcome};
use ag_harness::tool::ToolSet;
use tempfile::TempDir;

use super::coverage::{self, Coverage};
use super::fixture::{refusing_harness, schema};

/// The linked worktree's branch, nested like a host's session branches so
/// that Linux, which grants the directory holding it, keeps `main`
/// read-only.
const SESSION_BRANCH: &str = "wt/session";

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
            &["worktree", "add", "-q", "-b", SESSION_BRANCH, "../session"],
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
        .with_git_writes(SESSION_BRANCH.into())
        .expect("Git writes")
        .with_temporary_directory(self.temporary())
        .expect("temporary directory")
    }

    /// Creates `name` beside the worktree and returns it with the
    /// development configuration plus a read grant for it.
    fn granted_directory(&self, name: &str) -> (PathBuf, BashConfig) {
        let directory = self.root().join(name);
        std::fs::create_dir(&directory).expect("granted directory");
        let configuration = self
            .configuration()
            .with_read(directory.clone())
            .expect("read grant");

        (directory, configuration)
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
             then exit 93; fi; if printf changed > .git; then exit 94; fi; admin=$(git rev-parse \
             --absolute-git-dir); if /bin/mv \"$admin\" \"$common/objects/moved\"; then exit 95; \
             fi; if /bin/mv \"$common/worktrees\" \"$common/objects/moved\"; then exit 96; fi; \
             printf confined",
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
/// command; it now grants every directory by path without looking inside.
#[tokio::test]
async fn native_grants_are_not_inspected() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let (toolchain, configuration) = fixture.granted_directory("toolchain");
    std::fs::create_dir(toolchain.join("bin")).expect("toolchain");
    std::fs::write(toolchain.join("bin/tool"), "tool-value").expect("toolchain file");
    // Negative controls: any walk of a grant would trip over an entry it
    // cannot read or a FIFO, and the earlier walk refused both.
    for root in [&toolchain, &fixture.worktree()] {
        let locked = root.join("locked");
        std::fs::create_dir(&locked).expect("unreadable directory");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000))
            .expect("lock directory");
        let fifo = Command::new("/usr/bin/mkfifo")
            .arg(root.join("fifo"))
            .status()
            .expect("FIFO");
        assert!(fifo.success());
    }

    // Act
    let result = fixture
        .run(
            &format!(
                "set -e; /bin/cat '{}'; printf written > output/written",
                toolchain.join("bin/tool").display()
            ),
            configuration,
        )
        .await;
    for root in [&toolchain, &fixture.worktree()] {
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o700))
            .expect("unlock directory");
    }

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "tool-value", "{result:?}");
    assert!(fixture.worktree().join("output/written").exists());
}

/// Seatbelt denies every hard link a command tries to create: from a file
/// readable through a grant or from one not granted at all, into the
/// worktree or the temporary directory. Each attempt must fail with
/// Seatbelt's `Operation not permitted`, so a failure for another reason
/// does not pass. The files sit on the worktree's filesystem, so the
/// attempts do not fail merely for crossing devices on the host. On Linux
/// the mount layout is what refuses these links (`EXDEV` across binds,
/// `ENOENT` for ungranted paths), which
/// `access::native_linux_denies_writes_links_and_renames_outside_grants`
/// already covers.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_macos_denies_hard_links_to_files_outside_the_grants() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let ungranted = fixture.root().join("ungranted");
    std::fs::write(&ungranted, "ungranted-value").expect("ungranted file");
    let (readable, configuration) = fixture.granted_directory("readable");
    let granted = readable.join("granted");
    std::fs::write(&granted, "granted-value").expect("granted file");

    // Act
    let result = fixture
        .run(
            &format!(
                "set -e; [ -n \"$TMPDIR\" ] || exit 90; denied() {{ if error=$(/bin/ln \"$1\" \
                 \"$2\" 2>&1); then exit 91; fi; case \"$error\" in *'Operation not permitted'*) \
                 ;; *) printf '%s\\n' \"$error\" >&2; exit 92;; esac; }}; /bin/cat '{granted}'; \
                 denied '{granted}' granted-link; denied '{granted}' \"$TMPDIR/granted-link\"; \
                 denied '{ungranted}' ungranted-link; denied '{ungranted}' \
                 \"$TMPDIR/ungranted-link\"; /bin/ln -s tracked symbolic; /bin/cat symbolic",
                granted = granted.display(),
                ungranted = ungranted.display(),
            ),
            configuration,
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "granted-valuetracked-value", "{result:?}");
    for (path, contents) in [(&granted, "granted-value"), (&ungranted, "ungranted-value")] {
        assert_eq!(std::fs::metadata(path).expect("outside file").nlink(), 1);
        assert_eq!(
            std::fs::read_to_string(path).expect("outside file"),
            contents
        );
    }
    for name in ["granted-link", "ungranted-link"] {
        assert!(!fixture.worktree().join(name).exists());
        assert!(!fixture.temporary().join(name).exists());
    }
}

/// A dynamically linked compiler installed outside the system directories,
/// such as rustup's under the home directory, starts from a read grant of
/// its path, and does not start without it. The suite's own toolchain is
/// present wherever it runs: CI installs it with rustup under the home
/// directory. A toolchain under a system directory would make the grant add
/// nothing, so the test refuses that host instead of passing vacuously.
#[tokio::test]
async fn native_runs_a_toolchain_granted_by_path() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let sysroot = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("host compiler");
    assert!(sysroot.status.success(), "{sysroot:?}");
    let sysroot = PathBuf::from(String::from_utf8(sysroot.stdout).expect("sysroot").trim())
        .canonicalize()
        .expect("canonical sysroot");
    let system_root = SYSTEM_READ_ROOTS
        .iter()
        .find(|root| sysroot.starts_with(root));
    assert!(
        system_root.is_none(),
        "the toolchain at {} lies under the system read root {system_root:?}, so this test cannot \
         show its read grant matters; run it with a rustup toolchain outside the system \
         directories",
        sysroot.display()
    );
    let compiler = sysroot.join("bin/rustc");
    let expected = Command::new(&compiler)
        .arg("--version")
        .output()
        .expect("host compiler version");
    assert!(expected.status.success(), "{expected:?}");
    let command = format!("'{}' --version", compiler.display());
    let configuration = fixture
        .configuration()
        .with_read(sysroot)
        .expect("toolchain grant");

    // Act
    let ungranted = fixture.run(&command, fixture.configuration()).await;
    let result = fixture.run(&command, configuration).await;

    // Assert
    assert_ne!(ungranted.exit_code, Some(0), "{ungranted:?}");
    assert_eq!(ungranted.stdout, "", "{ungranted:?}");
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(
        result.stdout,
        String::from_utf8(expected.stdout).expect("version"),
        "{result:?}"
    );
    assert!(result.stdout.starts_with("rustc "), "{result:?}");
}

/// Roots that cover the directories `BashConfig::with_system_reads` grants.
#[cfg(target_os = "macos")]
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/System",
    "/Library/Developer",
    "/Library/Apple",
    "/Applications/Xcode.app",
    "/private/etc",
    "/private/var/db",
    "/private/var/select",
];

/// Roots that cover the directories `BashConfig::with_system_reads` grants.
#[cfg(target_os = "linux")]
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/bin", "/sbin", "/usr", "/lib", "/lib32", "/lib64", "/libx32", "/etc",
];

/// An earlier sandbox spent 3.3 to 3.9 s walking its grants before each
/// `git status` on macOS. The fastest of three runs filters out a loaded
/// runner, while a cost paid by every command still shows; the bound sits
/// far above a normal launch and well below that regression.
#[tokio::test]
async fn native_git_command_latency_stays_bounded() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let mut durations = Vec::new();

    // Act
    for _ in 0..3 {
        let started = Instant::now();
        let result = fixture
            .run("git status --porcelain", fixture.configuration())
            .await;
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        durations.push(started.elapsed());
    }

    // Assert
    let fastest = durations.iter().min().expect("three runs");
    assert!(
        *fastest < Duration::from_millis(1500),
        "every sandboxed git status took at least 1.5 s: {durations:?}"
    );
}

/// In a main worktree macOS commits while the Git directory, its protected
/// entries, other branches' reflogs, and the top-level entries Git does not
/// write as worktree state stay in place; Linux refuses Git writes beside a
/// write grant containing the Git directory, because Landlock would open all of
/// it.
#[tokio::test]
async fn native_git_writes_in_a_main_worktree_keep_the_git_directory_in_place() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let main = fixture.root().join("main");
    std::fs::create_dir(main.join("output")).expect("profile parent");
    let coverage = Coverage::new(&main);
    let configuration = |write: &str| {
        BashConfig::new(
            coverage
                .as_ref()
                .map_or_else(coverage::launcher, Coverage::launcher),
            "/bin/bash".into(),
            "main-worktree-v1".into(),
            Duration::from_secs(10),
            4096,
        )
        .expect("configuration")
        .with_host_information()
        .with_system_reads()
        .with_read(coverage::launcher())
        .expect("launcher grant")
        .with_write(write.into())
        .expect("worktree grant")
        .with_git_writes("main".into())
        .expect("Git writes")
        .with_temporary_directory(fixture.temporary())
        .expect("temporary directory")
    };
    let configuration_before = std::fs::read(main.join(".git/config")).expect("configuration");

    // Act
    let output = refusing_harness(&main)
        .turn(
            "set -e; export GIT_AUTHOR_NAME=Sandbox GIT_AUTHOR_EMAIL=sandbox@example.com \
             GIT_COMMITTER_NAME=Sandbox GIT_COMMITTER_EMAIL=sandbox@example.com; printf main > \
             main-file; git add main-file; git -c commit.gpgsign=false commit -q -m staged; \
             printf more >> main-file; git -c commit.gpgsign=false commit -q -m sandboxed \
             main-file; if /bin/mv .git \"$TMPDIR/moved\"; then exit 91; fi; if /bin/mv .git \
             moved; then exit 92; fi; if /bin/mv .git/hooks .git/objects/hooks; then exit 93; fi; \
             if printf changed >> .git/config; then exit 94; fi; if printf x >> \
             .git/logs/refs/heads/wt/session; then exit 95; fi; /bin/mkdir -p remotes; if /bin/mv \
             remotes .git/remotes; then exit 96; fi; if printf x > .git/packed-refs.lock; then \
             exit 97; fi; printf confined",
            options(configuration(".")),
        )
        .await
        .expect("turn");
    // Without a write grant around the Git directory, Linux opens only the
    // object store and the branch's directories of a main worktree: objects
    // can be written, but staging cannot replace the index.
    #[cfg(target_os = "linux")]
    let stores = refusing_harness(&main)
        .turn(
            "set -e; printf stored | git hash-object -w --stdin; printf staged > output/staged; \
             if git add output/staged; then exit 91; fi; if printf changed >> .git/config; then \
             exit 92; fi",
            options(configuration("output")),
        )
        .await
        .expect("stores turn");

    // Assert
    assert_eq!(
        std::fs::read(main.join(".git/config")).expect("configuration"),
        configuration_before
    );
    #[cfg(target_os = "linux")]
    {
        assert_eq!(stores.output()["exit_code"], 0, "{:?}", stores.output());
        assert_eq!(
            git(
                &main,
                &["cat-file", "-p", "74a541093e166b498b707bc7e0c38b6a8c1aa56f"]
            ),
            "stored"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let result: CommandOutcome =
            serde_json::from_value(output.into_output()).expect("command result");
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert_eq!(result.stdout, "confined", "{result:?}");
        assert_eq!(
            git(&main, &["log", "-1", "--format=%s"]).trim(),
            "sandboxed"
        );
        assert!(!main.join(".git/remotes").exists());
        assert!(!main.join(".git/packed-refs.lock").exists());
    }
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            output.output()["status"],
            "rejected",
            "{:?}",
            output.output()
        );
        assert!(!main.join("main-file").exists());
    }
}

/// The system grant gives Linux commands a `/proc` of their own PID
/// namespace and a `/dev` without host devices.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn native_linux_system_reads_mount_a_private_proc_and_a_minimal_dev() {
    // Arrange
    let fixture = LinkedWorktree::new();

    // Act
    let result = fixture
        .run(
            &format!(
                "[ -e /proc/self/status ] || exit 91; if [ -e /proc/{} ]; then exit 92; fi; for \
                 device in /dev/*; do if [ -b \"$device\" ]; then exit 93; fi; done; printf \
                 private",
                std::process::id()
            ),
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "private", "{result:?}");
}

/// Replacement refs, grafts, the shallow boundary, object alternates, and
/// commit-graph files change the history Git run outside the sandbox shows
/// without changing the objects it pushes, so commands cannot write them,
/// whether they exist or not, nor move the directories holding them to swap
/// in replacements.
#[tokio::test]
async fn native_git_writes_keep_history_rewrites_read_only() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let common = fixture.root().join("main/.git");
    std::fs::write(common.join("objects/info/alternates"), "").expect("alternates");

    // Act
    let result = fixture
        .run(
            "set -e; common=$(git rev-parse --path-format=absolute --git-common-dir); \
             evil=$(printf evil | git hash-object -w --stdin); original=$(git rev-parse \
             HEAD:tracked); if git replace \"$original\" \"$evil\" 2>/dev/null; then exit 91; fi; \
             if printf x >> \"$common/objects/info/alternates\"; then exit 92; fi; if printf x > \
             \"$common/objects/info/http-alternates\"; then exit 93; fi; if printf x > \
             \"$common/info/grafts\"; then exit 94; fi; if printf x > \"$common/shallow\"; then \
             exit 95; fi; if mv \"$common/objects/info\" \"$common/objects/aside\" 2>/dev/null; \
             then exit 96; fi; if mv \"$common/objects\" \"$common/objects-aside\" 2>/dev/null; \
             then exit 97; fi; if mv \"$common/refs\" \"$common/refs-aside\" 2>/dev/null; then \
             exit 98; fi; if printf x > \"$common/objects/info/commit-graph\"; then exit 89; fi; \
             if mkdir \"$common/objects/info/commit-graphs\"; then exit 90; fi; printf confined",
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "confined", "{result:?}");
    assert_eq!(git(&fixture.worktree(), &["replace", "-l"]), "");
    assert_eq!(
        std::fs::read_to_string(common.join("objects/info/alternates")).expect("alternates"),
        ""
    );
    assert!(!common.join("info/grafts").exists());
    assert!(!common.join("shallow").exists());
    assert!(!common.join("objects/info/commit-graph").exists());
    assert!(!common.join("objects/info/commit-graphs").exists());
}

/// Commands commit to the session branch, even one only `packed-refs`
/// holds, and write its reflog, but cannot move the base branch, create
/// branches or tags, stash, or edit or rewrite `packed-refs`, and `git gc`
/// stops at its first lock, leaving the repository intact. Each refusal
/// must name the denied write, so a step failing for another reason, such
/// as a missing identity, fails the test. macOS also keeps the session
/// branch's siblings read-only; Linux grants the directory holding it.
#[tokio::test]
async fn native_git_writes_cover_only_the_session_branch() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let main = fixture.root().join("main");
    git(&main, &["branch", "wt/other", "main"]);
    git(&main, &["pack-refs", "--all"]);
    let common = main.join(".git");
    let packed_before = std::fs::read(common.join("packed-refs")).expect("packed references");
    let base = git(&main, &["rev-parse", "main"]);
    let sibling = if cfg!(target_os = "macos") {
        "refused git branch -f wt/other HEAD || exit 92; "
    } else {
        ""
    };

    // Act
    let result = fixture
        .run(
            &format!(
                "export GIT_AUTHOR_NAME=Sandbox GIT_AUTHOR_EMAIL=sandbox@example.com \
                 GIT_COMMITTER_NAME=Sandbox GIT_COMMITTER_EMAIL=sandbox@example.com \
                 GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=maintenance.auto GIT_CONFIG_VALUE_0=false; \
                 refused() {{ if \"$@\" 2> \"$TMPDIR/refusal\"; then echo \"ran: $*\" >&2; return \
                 1; fi; grep -qiE 'not permitted|permission denied|cannot save the current \
                 status' \"$TMPDIR/refusal\" || {{ cat \"$TMPDIR/refusal\" >&2; return 1; }}; }}; \
                 common=$(git rev-parse --path-format=absolute --git-common-dir); git -c \
                 commit.gpgsign=false commit -q --allow-empty -m own || exit 90; refused git \
                 update-ref refs/heads/main HEAD || exit 91; {sibling}refused sh -c 'printf x >> \
                 \"$1/packed-refs\"' sh \"$common\" || exit 93; refused git tag sandboxed || exit \
                 94; refused git checkout -q -b wt-new || exit 95; printf changed >> tracked; \
                 refused git stash || exit 96; grep -q changed tracked || exit 97; refused git \
                 pack-refs --all || exit 98; refused git gc -q || exit 99; printf confined"
            ),
            fixture.configuration(),
        )
        .await;

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "confined", "{result:?}");
    assert_eq!(
        git(&main, &["log", "-1", "--format=%s", SESSION_BRANCH]).trim(),
        "own"
    );
    assert_eq!(
        git(&main, &["log", "-g", "-1", "--format=%gs", SESSION_BRANCH]).trim(),
        "commit: own"
    );
    assert_eq!(git(&main, &["rev-parse", "main"]), base);
    assert_eq!(git(&main, &["rev-parse", "wt/other"]), base);
    assert_eq!(
        std::fs::read(common.join("packed-refs")).expect("packed references"),
        packed_before
    );
    assert_eq!(git(&main, &["tag", "--list"]), "");
    assert_eq!(git(&main, &["branch", "--list", "wt-new"]), "");
    assert_eq!(git(&main, &["stash", "list"]), "");
    git(&main, &["fsck", "--no-progress"]);
}

/// Commands use the session's temporary directory as usual but cannot move
/// or remove the directory itself, and a link left in its place is refused
/// rather than followed, so every command writes the directory the host
/// granted.
#[tokio::test]
async fn native_temporary_directory_stays_the_granted_directory() {
    // Arrange
    let fixture = LinkedWorktree::new();
    let elsewhere = fixture.root().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("other directory");

    // Act
    let result = fixture
        .run(
            "mkdir -p \"$TMPDIR/a/b\" && file=$(mktemp \"$TMPDIR/x.XXXX\") && rm -rf \
             \"$TMPDIR/a\" \"$file\" || exit 90; if mv \"$TMPDIR\" moved-tmp 2>/dev/null; then \
             exit 91; fi; if rmdir \"$TMPDIR\" 2>/dev/null; then exit 92; fi; printf kept",
            fixture.configuration(),
        )
        .await;
    std::fs::rename(fixture.temporary(), fixture.root().join("aside")).expect("move aside");
    std::os::unix::fs::symlink(&elsewhere, fixture.temporary()).expect("link in place");
    let linked = refusing_harness(&fixture.worktree())
        .turn(
            "printf planted > \"$TMPDIR/planted\"",
            options(fixture.configuration()),
        )
        .await
        .expect("linked turn");

    // Assert
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "kept", "{result:?}");
    assert!(!fixture.worktree().join("moved-tmp").exists());
    assert_eq!(
        linked.output()["status"],
        "rejected",
        "{:?}",
        linked.output()
    );
    assert!(!elsewhere.join("planted").exists());
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

    let command = format!(
        "set -e; exec 3<>/dev/tcp/127.0.0.1/{port}; printf ping >&3; read -r -n 4 reply <&3; \
         printf %s \"$reply\""
    );

    // Act
    let denied = fixture.run(&command, fixture.configuration()).await;
    let result = fixture
        .run(&command, fixture.configuration().with_network())
        .await;

    // Assert
    assert_ne!(denied.exit_code, Some(0), "{denied:?}");
    assert_eq!(denied.stdout, "", "{denied:?}");
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "pong", "{result:?}");
    assert_eq!(&server.join().expect("listener thread"), b"ping");
}

#[tokio::test]
async fn native_refused_launch_is_answered_and_runs_once_the_host_fixes_it() {
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

    let harness = refusing_harness(&fixture.worktree());

    // Act
    let output = harness
        .turn("printf escaped > escaped", options(configuration.clone()))
        .await
        .expect("a refused launch is answered, not fatal");
    let missing_after_refusal = missing.exists();
    std::fs::create_dir(&missing).expect("host fixes the configuration");
    let fixed = harness
        .turn("printf \"$TMPDIR\"", options(configuration))
        .await
        .expect("fixed turn");

    // Assert
    assert_eq!(fixed.output()["exit_code"], 0, "{:?}", fixed.output());
    assert_eq!(fixed.output()["stdout"], missing.display().to_string());
    assert!(!missing_after_refusal);
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
}
