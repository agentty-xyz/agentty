use std::fs;
use std::path::Path;

/// Production workflows may depend on worker submission contracts, but only
/// the worker and runtime crates may name raw execution adapters.
#[test]
fn llm_execution_cannot_bypass_the_worker_boundary() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut violations = Vec::new();
    // Act
    inspect(&root, &root, &mut violations);
    // Assert
    assert!(
        violations.is_empty(),
        "LLM execution bypasses worker ownership: {violations:?}"
    );
}

fn inspect(root: &Path, directory: &Path, violations: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("source directory") {
        let path = entry.expect("source entry").path();
        let name = path.file_stem().expect("source name").to_string_lossy();
        if name.ends_with("_test") || name == "test_support" {
            continue;
        }
        if path.is_dir() {
            inspect(root, &path, violations);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let relative = path.strip_prefix(root).expect("relative source");
        let source = fs::read_to_string(&path).expect("source text");
        let compact: String = source
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        {
            for forbidden in [
                "OneShotClient",
                "AgentChannel",
                "create_agent_channel",
                "create_app_server_client",
            ] {
                if source.contains(forbidden) {
                    violations.push(format!("{}: {forbidden}", relative.display()));
                }
            }
        }
        // Runtime turn calls are owned by ag-worker. Agentty may invoke the
        // session client, but must not invoke the raw trait method or UFCS.
        for forbidden in [
            ".run_turn(",
            ".shutdown_session(",
            "ag_worker::run_turn",
            "AgentChannel::run_turn",
            "OneShotClient::submit",
        ] {
            if compact.contains(forbidden) {
                violations.push(format!("{}: {forbidden}", relative.display()));
            }
        }
    }
}

/// Cargo metadata resolves aliases and includes optional, target-specific,
/// normal, build, and development dependencies before checking ownership.
#[test]
fn execution_crate_dependencies_follow_the_layering_contract() {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace");
    let output = std::process::Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()
        .expect("workspace metadata");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("metadata JSON");
    let mut violations = Vec::new();
    // Act
    for package in metadata["packages"].as_array().expect("packages") {
        let owner = package["name"].as_str().expect("package name");
        for dependency in package["dependencies"].as_array().expect("dependencies") {
            let target = dependency["name"].as_str().expect("dependency name");
            if !allowed_execution_dependency(owner, target) {
                violations.push(format!("{owner} -> {target}"));
            }
        }
    }
    // Assert
    assert!(
        violations.is_empty(),
        "Forbidden execution dependencies: {violations:?}"
    );
}

#[test]
fn dependency_policy_rejects_bypasses_and_keeps_contracts_independent() {
    // Arrange / Act / Assert
    for (owner, target, allowed) in [
        ("ag-worker", "ag-runtime", true),
        ("ag-runtime", "ag-agent", true),
        ("agentty", "ag-worker", true),
        ("ag-store", "ag-worker", true),
        ("ag-session", "ag-worker", false),
        ("ag-runtime", "ag-worker", false),
        ("ag-agent", "ag-worker", false),
        ("agentty", "ag-runtime", false),
        ("agentty", "ag-agent", false),
        ("ag-agent", "ag-runtime", false),
        ("ag-session", "ag-runtime", false),
        ("ag-contracts", "ag-worker", false),
        ("ag-contracts", "ag-session", false),
        ("ag-contracts", "ag-protocol", true),
    ] {
        assert_eq!(
            allowed_execution_dependency(owner, target),
            allowed,
            "{owner} -> {target}"
        );
    }
}

fn allowed_execution_dependency(owner: &str, target: &str) -> bool {
    match target {
        "ag-worker" => matches!(owner, "agentty" | "ag-store"),
        "ag-agent" => owner == "ag-runtime",
        "ag-runtime" => owner == "ag-worker",
        _ => owner != "ag-contracts" || !target.starts_with("ag-") || target == "ag-protocol",
    }
}
