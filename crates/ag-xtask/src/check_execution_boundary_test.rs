use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::check_execution_boundary::{
    AGENT_EXECUTABLES, Dependency, ExecutionBoundaryCheck, MockWorkspace, RealWorkspace,
    SourceFile, Workspace, run,
};

fn dependency(owner: &str, target: &str) -> Dependency {
    Dependency {
        owner: owner.to_string(),
        target: target.to_string(),
    }
}

fn source(crate_name: &str, path: &str, text: &str) -> SourceFile {
    SourceFile {
        crate_name: crate_name.to_string(),
        path: PathBuf::from(path),
        text: text.to_string(),
    }
}

/// Returns an `ag-agent` source that launches every listed agent CLI.
fn adapter_source() -> SourceFile {
    let text = AGENT_EXECUTABLES
        .map(|executable| format!("Command::new(\"{executable}\");"))
        .concat();

    source("ag-agent", "ag-agent/src/agent.rs", &text)
}

fn workspace(dependencies: Vec<Dependency>, sources: Vec<SourceFile>) -> MockWorkspace {
    let mut workspace = MockWorkspace::new();
    workspace
        .expect_dependencies()
        .once()
        .return_once(move || Ok(dependencies));
    workspace
        .expect_production_sources()
        .withf(|crates| crates == Path::new("crates"))
        .once()
        .return_once(move |_| Ok(sources));

    workspace
}

fn check_result(workspace: &MockWorkspace) -> Result<(), String> {
    ExecutionBoundaryCheck { workspace }.run(Path::new("crates"))
}

#[test]
fn layered_workspace_passes() {
    // Arrange
    let workspace = workspace(
        vec![
            dependency("agentty", "ag-worker"),
            dependency("ag-worker", "ag-runtime"),
            dependency("ag-runtime", "ag-agent"),
            dependency("ag-harness-cli", "ag-harness"),
        ],
        vec![
            adapter_source(),
            source(
                "ag-worker",
                "ag-worker/src/turn.rs",
                "channel.run_turn(request); AgentChannel",
            ),
            source(
                "agentty",
                "agentty/src/app.rs",
                "client.run(request); Command::new(\"git\");",
            ),
        ],
    );

    // Act
    let result = check_result(&workspace);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn bypasses_are_reported_together() {
    // Arrange
    let workspace = workspace(
        vec![
            dependency("agentty", "ag-runtime"),
            dependency("ag-orchestration", "ag-harness"),
            dependency("ag-contracts", "ag-session"),
        ],
        vec![
            source(
                "ag-agent",
                "ag-agent/src/agent.rs",
                "Command::new(\"npx\");",
            ),
            source(
                "ag-orchestration",
                "ag-orchestration/src/plan.rs",
                "fn plan(channel: &dyn AgentChannel) { channel . run_turn ( request ); }",
            ),
            source(
                "ag-runtime",
                "ag-runtime/src/provider.rs",
                "tokio::process::Command::new( \"claude\" );",
            ),
        ],
    );

    // Act
    let result = check_result(&workspace);

    // Assert
    assert_eq!(
        result,
        Err([
            "Execution boundary violations:",
            "forbidden dependency agentty -> ag-runtime",
            "forbidden dependency ag-orchestration -> ag-harness",
            "forbidden dependency ag-contracts -> ag-session",
            "ag-orchestration/src/plan.rs: AgentChannel",
            "ag-orchestration/src/plan.rs: .run_turn(",
            "ag-runtime/src/provider.rs: launches agent CLI `claude` outside ag-agent",
            "ag-agent launches unlisted executable `npx`",
            "ag-agent no longer launches `agy`",
            "ag-agent no longer launches `claude`",
            "ag-agent no longer launches `codex`",
            "ag-agent no longer launches `gemini`",
        ]
        .join("\n"))
    );
}

#[test]
fn workspace_read_errors_fail_the_check() {
    // Arrange
    let mut dependency_failure = MockWorkspace::new();
    dependency_failure
        .expect_dependencies()
        .once()
        .return_once(|| Err("metadata unavailable".to_string()));
    let source_failure = {
        let mut workspace = MockWorkspace::new();
        workspace
            .expect_dependencies()
            .once()
            .return_once(|| Ok(Vec::new()));
        workspace
            .expect_production_sources()
            .once()
            .return_once(|_| Err("sources unavailable".to_string()));
        workspace
    };

    // Act
    let dependency_result = check_result(&dependency_failure);
    let source_result = check_result(&source_failure);

    // Assert
    assert_eq!(dependency_result, Err("metadata unavailable".to_string()));
    assert_eq!(source_result, Err("sources unavailable".to_string()));
}

#[test]
fn dependency_policy_rejects_bypasses_and_keeps_contracts_independent() {
    // Arrange / Act / Assert
    for (owner, target, allowed) in [
        ("ag-worker", "ag-runtime", true),
        ("ag-runtime", "ag-agent", true),
        ("agentty", "ag-worker", true),
        ("ag-store", "ag-worker", true),
        ("ag-harness-cli", "ag-harness", true),
        ("ag-session", "ag-worker", false),
        ("ag-runtime", "ag-worker", false),
        ("ag-agent", "ag-worker", false),
        ("agentty", "ag-runtime", false),
        ("agentty", "ag-agent", false),
        ("agentty", "ag-harness", false),
        ("ag-orchestration", "ag-harness", false),
        ("ag-agent", "ag-runtime", false),
        ("ag-session", "ag-runtime", false),
        ("ag-contracts", "ag-worker", false),
        ("ag-contracts", "ag-session", false),
        ("ag-contracts", "ag-protocol", true),
        ("ag-contracts", "serde", true),
    ] {
        assert_eq!(
            ExecutionBoundaryCheck::allowed_dependency(owner, target),
            allowed,
            "{owner} -> {target}"
        );
    }
}

#[test]
fn literal_command_launches_ignore_formatting_and_dynamic_programs() {
    // Arrange
    let text = r#"
        Command::new( "claude" );
        tokio::process::Command::new("git");
        Command::new(program);
        Command::new("unterminated
    "#;

    // Act
    let launches = ExecutionBoundaryCheck::literal_command_launches(text);

    // Assert
    assert_eq!(launches, ["claude", "git"]);
}

#[test]
fn real_workspace_reads_dependencies_from_cargo_metadata() {
    // Arrange
    let workspace = RealWorkspace {
        cargo: OsString::from(env!("CARGO")),
    };

    // Act
    let dependencies = workspace.dependencies().expect("workspace metadata");

    // Assert
    assert!(dependencies.contains(&dependency("ag-worker", "ag-runtime")));
    assert!(dependencies.contains(&dependency("ag-runtime", "ag-agent")));
}

#[test]
fn real_workspace_reports_cargo_metadata_failures() {
    // Arrange
    let failing = RealWorkspace {
        cargo: OsString::from("false"),
    };
    let missing = RealWorkspace {
        cargo: OsString::from("ag-xtask-missing-cargo"),
    };

    // Act
    let failing_result = failing.dependencies();
    let missing_result = missing.dependencies();

    // Assert
    assert_eq!(failing_result, Err("cargo metadata failed: ".to_string()));
    assert!(
        missing_result
            .expect_err("missing cargo")
            .starts_with("Failed to run cargo metadata:")
    );
}

#[test]
fn cargo_metadata_parsing_flattens_dependencies_and_rejects_invalid_json() {
    // Arrange
    let metadata = br#"{"packages": [
        {"name": "ag-worker", "dependencies": [{"name": "ag-runtime"}, {"name": "tokio"}]},
        {"name": "ag-protocol", "dependencies": []}
    ]}"#;

    // Act
    let dependencies = RealWorkspace::parse_dependencies(metadata);
    let invalid = RealWorkspace::parse_dependencies(b"{}");

    // Assert
    assert_eq!(
        dependencies,
        Ok(vec![
            dependency("ag-worker", "ag-runtime"),
            dependency("ag-worker", "tokio"),
        ])
    );
    assert!(
        invalid
            .expect_err("invalid metadata")
            .starts_with("Failed to parse cargo metadata:")
    );
}

#[test]
fn real_workspace_collects_sorted_production_sources() {
    // Arrange
    let directory = tempdir().expect("workspace");
    let crates = directory.path().join("crates");
    let source_root = crates.join("example/src");
    fs::create_dir_all(source_root.join("app")).expect("nested module");
    fs::create_dir_all(source_root.join("test_support")).expect("test support");
    fs::create_dir_all(crates.join("docs-only")).expect("crate without sources");
    fs::write(crates.join("README.md"), "Workspace crates").expect("non-crate entry");
    fs::write(source_root.join("lib.rs"), "mod app;").expect("crate root");
    fs::write(source_root.join("app/state.rs"), "struct State;").expect("nested source");
    fs::write(source_root.join("app_test.rs"), "AgentChannel").expect("test source");
    fs::write(source_root.join("test_support/fake.rs"), "AgentChannel").expect("fixture");
    fs::write(source_root.join("notes.md"), "AgentChannel").expect("non-Rust file");
    let workspace = RealWorkspace {
        cargo: OsString::from("cargo"),
    };

    // Act
    let sources = workspace.production_sources(&crates);

    // Assert
    assert_eq!(
        sources,
        Ok(vec![
            source("example", "example/src/app/state.rs", "struct State;"),
            source("example", "example/src/lib.rs", "mod app;"),
        ])
    );
}

#[test]
fn real_workspace_reports_unreadable_sources() {
    // Arrange
    let directory = tempdir().expect("workspace");
    let crates = directory.path().join("crates");
    let source_root = crates.join("example/src");
    fs::create_dir_all(&source_root).expect("source root");
    fs::write(source_root.join("lib.rs"), [0xff, 0xfe]).expect("invalid UTF-8 source");
    let workspace = RealWorkspace {
        cargo: OsString::from("cargo"),
    };

    // Act
    let invalid_source = workspace.production_sources(&crates);
    let missing_root = workspace.production_sources(&directory.path().join("missing"));

    // Assert
    assert!(
        invalid_source
            .expect_err("invalid source")
            .starts_with("Failed to read ")
    );
    assert!(
        missing_root
            .expect_err("missing root")
            .starts_with("Failed to read ")
    );
}

#[test]
fn production_composition_reports_a_missing_workspace_root() {
    // Arrange
    // Cargo runs unit tests in the package directory, outside the workspace
    // root.
    assert!(!Path::new("crates").exists());

    // Act
    let result = run();

    // Assert
    assert!(
        result
            .expect_err("missing workspace root")
            .starts_with("Failed to read crates:")
    );
}
