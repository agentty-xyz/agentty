//! Cataloged hook commands for process-level hook contracts.

use std::error::Error;
use std::io;

use serde_yaml_ng::Value;

/// Returns the `entry` command of the hook with `id` in
/// `.pre-commit-config.yaml`.
pub(super) fn entry(id: &str) -> Result<String, Box<dyn Error>> {
    let catalog: Value =
        serde_yaml_ng::from_str(include_str!("../../../../.pre-commit-config.yaml"))?;
    let hook = catalog["repos"]
        .as_sequence()
        .ok_or_else(|| io::Error::other("hook catalog has no repository list"))?
        .iter()
        .map(|repo| {
            repo["hooks"]
                .as_sequence()
                .ok_or_else(|| io::Error::other("hook repository has no hook list"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .find(|hook| hook["id"].as_str() == Some(id))
        .ok_or_else(|| io::Error::other(format!("hook catalog has no `{id}` hook")))?;

    Ok(hook["entry"]
        .as_str()
        .ok_or_else(|| io::Error::other(format!("`{id}` hook has no command")))?
        .to_string())
}
