//! Crate-wide `cargo metadata` fixtures for release tests.

use serde_json::{Value, json};

/// Workspace root reported by [`demo_metadata`].
pub(crate) const DEMO_ROOT: &str = "/workspace/demo";

/// Returns `cargo metadata --format-version 1 --no-deps` output for a demo
/// workspace with two apps, a library-only package, and every release key
/// set.
pub(crate) fn demo_metadata() -> Value {
    json!({
        "metadata": {
            "release": {
                "apps": {
                    "demo-app": {},
                    "demo-tool": {"npm-scope": "@demo-org"},
                },
                "include": ["README.md", "LICENSE"],
                "installers": ["shell", "npm"],
                "min-glibc": "2.35",
                "profile": "dist",
                "targets": {
                    "x86_64-unknown-linux-gnu": "ubuntu-22.04",
                    "aarch64-apple-darwin": "macos-latest",
                },
            },
        },
        "packages": [
            package("demo-lib", &[("demo_lib", "lib")]),
            package(
                "demo-app",
                &[
                    ("demo_app", "lib"),
                    ("demo-helper", "bin"),
                    ("demo-app", "bin"),
                    ("contract", "test"),
                ],
            ),
            package("demo-tool", &[("demo-tool", "bin")]),
        ],
        "target_directory": "/workspace/demo/target",
        "version": 1,
        "workspace_members": [],
        "workspace_root": DEMO_ROOT,
    })
}

/// Returns `metadata` serialized as `cargo metadata` prints it.
pub(crate) fn metadata_bytes(metadata: &Value) -> Vec<u8> {
    serde_json::to_vec(metadata).expect("serialize metadata")
}

/// Returns one demo workspace member with the given `(name, kind)` targets.
fn package(name: &str, targets: &[(&str, &str)]) -> Value {
    json!({
        "authors": ["Demo Author <demo@example.com>"],
        "description": "Demo package",
        "id": format!("path+file:///workspace/demo/crates/{name}#1.2.3"),
        "license": "Apache-2.0",
        "manifest_path": format!("/workspace/demo/crates/{name}/Cargo.toml"),
        "metadata": null,
        "name": name,
        "repository": "https://github.com/demo-org/demo-app",
        "targets": targets
            .iter()
            .map(|(target, kind)| json!({"kind": [kind], "name": target}))
            .collect::<Vec<_>>(),
        "version": "1.2.3",
    })
}
