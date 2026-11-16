//! Container run orchestration, settings resolution, and process host tests.

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use crate::e2e_container::{Guard, Host, ProcessHost, Runner, Settings, cache_root, pinned_image};

const PINNED: &str = "ghcr.io/example/e2e@sha256:abc";

struct ScriptedGuard;
impl Guard for ScriptedGuard {}

/// Records boundary calls and fails the operation named by `error`.
struct ScriptedHost {
    calls: RefCell<Vec<String>>,
    empty: Vec<&'static str>,
    error: &'static str,
}

impl ScriptedHost {
    fn new() -> Self {
        Self {
            calls: RefCell::default(),
            empty: Vec::new(),
            error: "",
        }
    }

    fn record(&self, call: String) -> Result<(), String> {
        let succeeds = self.error.is_empty() || !call.starts_with(self.error);
        self.calls.borrow_mut().push(call.clone());
        if succeeds { Ok(()) } else { Err(call) }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Host for ScriptedHost {
    fn output(&self, program: &str, arguments: &[&str]) -> Result<String, String> {
        self.record(format!("{program} {}", arguments.join(" ")))?;

        Ok(if arguments == ["-u"] { "501\n" } else { "20\n" }.into())
    }

    fn lock(&self, cache: &Path, target: &str) -> Result<Box<dyn Guard>, String> {
        self.record(format!("lock {} {target}", cache.display()))?;

        Ok(Box::new(ScriptedGuard))
    }

    fn status(&self, program: &str, arguments: &[&str]) -> Result<(), String> {
        self.record(format!("{program} {}", arguments.join(" ")))
    }

    fn pages(&self, directory: &Path) -> Result<Vec<String>, String> {
        self.record(format!("pages {}", directory.display()))?;

        Ok(vec![
            "+++\ntitle = \"Index\"\n+++\n".into(),
            "+++\n[extra]\ngif = \"empty.gif\"\n+++\n".into(),
            "+++\n[extra]\ngif = \"present.gif\"\n+++\n".into(),
        ])
    }

    fn is_nonempty(&self, path: &Path) -> bool {
        !self
            .empty
            .iter()
            .any(|name| path == Path::new("/workspace/docs/site/static/features").join(name))
    }
}

fn settings() -> Settings {
    Settings {
        architecture: "aarch64",
        cache: "/cache-root".into(),
        candidate: None,
        filter: Some("test(help)".into()),
        mode: "generate".into(),
        pinned: PINNED.into(),
        workspace: "/workspace".into(),
    }
}

#[test]
fn pinned_runs_pull_inside_the_lock_and_run_the_filtered_target() {
    // Arrange
    let host = ScriptedHost::new();

    // Act
    let result = Runner { host: &host }.run(&settings());

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(
        host.calls(),
        [
            "id -u".to_string(),
            "id -g".into(),
            "lock /cache-root target-c52ddf65".into(),
            format!("podman pull --platform linux/arm64 {PINNED}"),
            format!(
                "podman run --rm --pull=never --platform linux/arm64 --user 501:20 --mount \
                 type=bind,source=/workspace,target=/workspace --mount \
                 type=bind,source=/cache-root,target=/cache --env HOME=/cache/home --env \
                 CARGO_HOME=/cache/cargo --env PREK_HOME=/cache/prek --env \
                 CARGO_TARGET_DIR=/cache/target-c52ddf65 --env CARGO_BUILD_JOBS=1 --env \
                 CARGO_PROFILE_DEV_DEBUG=0 --env TESTTY_GIF_MODE=generate {PINNED} cargo nextest \
                 run --locked --profile ci -p agentty --test e2e -E test(help)"
            ),
            "pages /workspace/docs/site/content/features".into(),
        ]
    );
}

#[test]
fn candidate_images_run_without_pulling_or_filtering() {
    // Arrange
    let host = ScriptedHost::new();
    let settings = Settings {
        architecture: "x86_64",
        candidate: Some("sha256:candidate".into()),
        filter: None,
        ..settings()
    };

    // Act
    let result = Runner { host: &host }.run(&settings);

    // Assert
    assert_eq!(result, Ok(()));
    let calls = host.calls();
    assert!(!calls.iter().any(|call| call.starts_with("podman pull")));
    assert!(calls[3].starts_with("podman run --rm --pull=never --platform linux/amd64 "));
    assert!(calls[3].ends_with(
        " sha256:candidate cargo nextest run --locked --profile ci -p agentty --test e2e"
    ));
}

#[test]
fn each_failure_stops_the_remaining_steps() {
    for (error, completed) in [
        ("id -u", 1),
        ("id -g", 2),
        ("lock", 3),
        ("podman pull", 4),
        ("podman run", 5),
        ("pages", 6),
    ] {
        // Arrange
        let mut host = ScriptedHost::new();
        host.error = error;

        // Act
        let result = Runner { host: &host }.run(&settings());

        // Assert
        assert!(result.is_err(), "{error}");
        assert_eq!(host.calls().len(), completed, "{error}");
    }
}

#[test]
fn unsupported_architectures_fail_before_any_process_runs() {
    // Arrange
    let host = ScriptedHost::new();
    let settings = Settings {
        architecture: "riscv64",
        ..settings()
    };

    // Act
    let result = Runner { host: &host }.run(&settings);

    // Assert
    assert_eq!(
        result,
        Err("Unsupported E2E container architecture: riscv64".into())
    );
    assert_eq!(host.calls(), Vec::<String>::new());
}

#[test]
fn declared_gifs_that_are_missing_or_empty_fail_the_run() {
    // Arrange
    let mut host = ScriptedHost::new();
    host.empty = vec!["empty.gif"];

    // Act
    let result = Runner { host: &host }.run(&settings());

    // Assert
    assert_eq!(
        result,
        Err("Feature GIFs are missing or empty: empty.gif".into())
    );
}

#[test]
fn cache_root_prefers_xdg_and_treats_empty_values_as_unset() {
    // Arrange
    let xdg = Some("/xdg".into());
    let home = Some("/home/user".into());

    // Act
    let preferred = cache_root(xdg, home.clone());
    let fallback = cache_root(Some("".into()), home);
    let missing = cache_root(None, Some("".into()));

    // Assert
    assert_eq!(preferred, Ok(PathBuf::from("/xdg/agentty-e2e")));
    assert_eq!(fallback, Ok(PathBuf::from("/home/user/.cache/agentty-e2e")));
    assert!(missing.is_err());
}

#[test]
fn pinned_image_requires_a_digest_in_the_workflow_container() {
    // Arrange
    let pinned = format!("jobs:\n  test:\n    container:\n      image: {PINNED}\n");

    // Act
    let resolved = pinned_image(&pinned);
    let tagged = pinned_image("jobs:\n  test:\n    container:\n      image: e2e:latest\n");
    let malformed = pinned_image("jobs: [");

    // Assert
    assert_eq!(resolved, Ok(PINNED.into()));
    assert!(tagged.is_err());
    assert!(malformed.is_err());
}

#[test]
fn process_host_reports_command_output_and_failures() {
    // Arrange
    let host = ProcessHost;
    let missing = "agentty-e2e-container-missing-program";

    // Act
    let output = host.output("echo", &["501"]);
    let failed_output = host.output("false", &[]);
    let missing_output = host.output(missing, &[]);
    let status = host.status("true", &[]);
    let failed_status = host.status("false", &[]);
    let missing_status = host.status(missing, &[]);

    // Assert
    assert_eq!(output, Ok("501\n".into()));
    assert!(failed_output.is_err());
    assert!(missing_output.is_err_and(|error| error.contains(missing)));
    assert_eq!(status, Ok(()));
    assert!(failed_status.is_err());
    assert!(missing_status.is_err_and(|error| error.contains(missing)));
}

#[test]
fn process_host_lock_prepares_cache_directories() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let cache = directory.path().join("agentty-e2e");

    // Act
    let guard = ProcessHost.lock(&cache, "target-checkout");

    // Assert
    assert!(guard.is_ok());
    for name in ["home", "cargo", "prek", "target-checkout", "container.lock"] {
        assert!(cache.join(name).exists(), "{name}");
    }
}

#[test]
fn process_host_reads_markdown_pages_and_gif_sizes() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path();
    fs::write(path.join("feature.md"), "gif = \"feature.gif\"").expect("page");
    fs::write(path.join("feature.gif"), "GIF").expect("gif");
    fs::write(path.join("empty.gif"), "").expect("empty gif");

    // Act
    let pages = ProcessHost.pages(path);
    let unreadable = ProcessHost.pages(&path.join("missing"));

    // Assert
    assert_eq!(pages, Ok(vec!["gif = \"feature.gif\"".to_string()]));
    assert!(unreadable.is_err());
    assert!(ProcessHost.is_nonempty(&path.join("feature.gif")));
    assert!(!ProcessHost.is_nonempty(&path.join("empty.gif")));
    assert!(!ProcessHost.is_nonempty(&path.join("missing.gif")));
}
