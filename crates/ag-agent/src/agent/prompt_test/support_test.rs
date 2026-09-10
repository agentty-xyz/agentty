use std::path::Path;

/// Returns the workspace root used by prompt preparation tests.
pub(super) fn test_workspace_root() -> &'static Path {
    Path::new("/tmp/agentty-wt/session-1")
}

/// Collapses rendered prompt whitespace for semantic assertions.
pub(super) fn normalize_prompt(prompt: &str) -> String {
    prompt.split_whitespace().collect::<Vec<_>>().join(" ")
}
