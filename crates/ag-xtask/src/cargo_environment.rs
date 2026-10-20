//! Keep parent-package metadata out of independent child builds.

use std::process::Command;

/// Creates a command without package metadata injected by `cargo run`.
///
/// Child Cargo invocations populate these values for their own packages. Keep
/// user configuration, including cache locations and compiler wrappers, intact.
pub(crate) fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        let is_package_metadata = name.to_str().is_some_and(|name| {
            name.starts_with("CARGO_PKG_")
                || matches!(
                    name,
                    "CARGO_MANIFEST_DIR" | "CARGO_MANIFEST_PATH" | "CARGO_MANIFEST_LINKS"
                )
        });
        if is_package_metadata {
            command.env_remove(name);
        }
    }

    command
}
