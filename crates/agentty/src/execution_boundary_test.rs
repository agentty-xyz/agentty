use std::fs;
use std::path::Path;

/// Production workflows may depend on worker submission contracts, but only
/// the composition module may name the raw isolated runtime or its factory.
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
        if relative != Path::new("app/service.rs") {
            for forbidden in [
                "OneShotClient",
                "create_agent_channel",
                "create_app_server_client",
            ] {
                if source.contains(forbidden) {
                    violations.push(format!("{}: {forbidden}", relative.display()));
                }
            }
        }
        // Runtime turn calls are owned by ag-worker. Agentty may invoke the
        // worker function, but must not invoke the raw trait method or UFCS.
        for forbidden in [
            ".run_turn(",
            "AgentChannel::run_turn",
            "OneShotClient::submit",
        ] {
            if compact.contains(forbidden) {
                violations.push(format!("{}: {forbidden}", relative.display()));
            }
        }
    }
}
