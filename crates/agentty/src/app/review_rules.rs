//! Bounded project review criteria selected by changed paths and file types.

use std::collections::BTreeSet;
use std::path::Path;

use ag_contracts::OneShotError;
use ag_git::DiffFile;
use serde::Deserialize;

use crate::infra::fs::{FsClient, FsError};

/// Leaves most of the prompt budget available for source, history, and
/// findings.
const MAX_CRITERIA_BYTES: usize = 8_000;

/// Optional project configuration, added to the built-in review criteria.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReviewRules {
    rules: Vec<ReviewRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRule {
    #[serde(default)]
    extensions: Vec<String>,
    instructions: String,
    #[serde(default)]
    path_prefix: String,
}

impl ReviewRules {
    /// Loads optional repository configuration through the injected boundary.
    /// Missing configuration uses built-ins; invalid configuration fails
    /// visibly.
    pub(super) async fn load(fs: &dyn FsClient, folder: &Path) -> Result<Self, OneShotError> {
        let bytes = match fs
            .read_file_prefix(folder.join(".agentty/review-rules.json"), 65_537)
            .await
        {
            Ok(bytes) => bytes,
            Err(FsError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(OneShotError::new(format!(
                    "Cannot load review rules: {error}"
                )));
            }
        };
        if bytes.len() > 65_536 {
            return Err(OneShotError::new("Project review rules exceed 65536 bytes"));
        }
        let rules: Self = serde_json::from_slice(&bytes)
            .map_err(|error| OneShotError::new(format!("Invalid project review rules: {error}")))?;
        for rule in &rules.rules {
            if rule.instructions.trim().is_empty()
                || rule.path_prefix.starts_with('/')
                || rule.path_prefix.contains('\\')
                || rule
                    .path_prefix
                    .split('/')
                    .any(|part| part == ".." || part == ".")
                || rule
                    .extensions
                    .iter()
                    .any(|extension| extension.is_empty() || extension.contains(['/', '\\', '.']))
            {
                return Err(OneShotError::new(
                    "Review rules need nonempty instructions, relative path prefixes, and \
                     extensions without dots or separators",
                ));
            }
        }

        Ok(rules)
    }

    /// Renders only criteria relevant to the supplied changed files.
    /// Project text is JSON-encoded review data, never execution policy.
    /// Each matching rule is rendered once, with its original filters intact.
    /// Oversized selected criteria fail before any provider work is submitted.
    pub(super) fn for_diff(&self, diff: &str) -> Result<String, OneShotError> {
        let paths: BTreeSet<_> = DiffFile::parse(diff)
            .into_iter()
            .flat_map(|file| [file.old_path, file.new_path])
            .filter(|path| !path.is_empty())
            .collect();
        let mut criteria = Vec::new();
        for path in &paths {
            let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
            let language = match extension {
                "rs" => {
                    "Rust: check ownership and lifetimes, async cancellation, locks held across \
                     awaits, error propagation, and exhaustive state transitions. Trace compound \
                     conditions and dispatch through immediate/deferred, paused/active, and \
                     empty/nonempty paths."
                }
                "sql" => {
                    "SQL: check transaction atomicity, parameter binding, constraints, migration \
                     ordering, and compatibility with existing data."
                }
                "js" | "jsx" | "ts" | "tsx" => {
                    "JavaScript/TypeScript: check async errors, nullability, stale state, resource \
                     cleanup, and validation at trust boundaries."
                }
                "py" => {
                    "Python: check mutable defaults, exception paths, resource cleanup, and input \
                     validation."
                }
                "go" => {
                    "Go: check error handling, goroutine cancellation, synchronization, and \
                     deferred resource cleanup."
                }
                "toml" | "yaml" | "yml" | "json" => {
                    "Configuration: check that referenced names and paths exist, defaults preserve \
                     intended behavior, and consumers accept the new configuration."
                }
                _ => {
                    "Check changed contracts against callers and dependent files; investigate \
                     concrete correctness and reliability risks."
                }
            };
            if !criteria.iter().any(|criterion| criterion == language) {
                criteria.push(language.to_string());
            }
            if (path.starts_with("tests/")
                || path.contains("/tests/")
                || path.contains("_test.")
                || path.contains(".test."))
                && !criteria
                    .iter()
                    .any(|criterion| criterion.starts_with("Tests:"))
            {
                criteria.push(
                    "Tests: check that assertions exercise the changed public behavior, failures \
                     cannot pass silently, and mocks preserve relevant contracts. Verify setup \
                     preconditions, fixture replacements, and no-op/default-success paths."
                        .to_string(),
                );
            }
        }
        for rule in &self.rules {
            let prefix = rule.path_prefix.trim_end_matches('/');
            if paths.iter().any(|path| {
                let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);

                (prefix.is_empty()
                    || path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('/')))
                    && (rule.extensions.is_empty()
                        || rule
                            .extensions
                            .iter()
                            .any(|candidate| candidate == extension))
            }) {
                let entry = format!(
                    "Project criterion (path_prefix: {}; extensions: {}): {}",
                    serde_json::json!(rule.path_prefix),
                    serde_json::json!(rule.extensions),
                    rule.instructions
                );
                if !criteria.contains(&entry) {
                    criteria.push(entry);
                }
            }
        }
        let rendered = serde_json::json!(criteria).to_string();
        if rendered.len() > MAX_CRITERIA_BYTES {
            return Err(OneShotError::new(format!(
                "Selected review criteria exceed {MAX_CRITERIA_BYTES} bytes; shorten project \
                 instructions or narrow the review scope"
            )));
        }

        Ok(rendered)
    }
}

#[cfg(test)]
#[path = "review_rules_test.rs"]
mod tests;
