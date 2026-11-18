//! Crate-wide fixtures for release tests: `cargo metadata` output of a demo
//! workspace and host-boundary doubles.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::{Value, json};

use crate::file_system::MockFileSystem;
use crate::process::MockProcessRunner;

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

/// Returns a process runner that answers one `cargo metadata` call with
/// `metadata`; tests add expectations for further calls.
pub(crate) fn metadata_process(metadata: &Value) -> MockProcessRunner {
    let metadata = metadata_bytes(metadata);
    let mut process = MockProcessRunner::new();
    process
        .expect_output()
        .withf(|command| command.args.first().is_some_and(|arg| arg == "metadata"))
        .once()
        .return_once(move |_| Ok(metadata));

    process
}

/// Writer whose every write and flush fails with a broken pipe.
pub(crate) struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

/// Files of a [`memory_file_system`] by path, shared with the test that
/// inspects what a command wrote.
pub(crate) type MemoryFiles = Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>;

/// Returns `files` as shared [`MemoryFiles`].
pub(crate) fn memory_files(files: &[(&str, &[u8])]) -> MemoryFiles {
    Arc::new(Mutex::new(
        files
            .iter()
            .map(|(path, contents)| (PathBuf::from(path), contents.to_vec()))
            .collect(),
    ))
}

/// Returns a file system that serves and records `files` in memory: `is_file`
/// and `read` look paths up, and `write` stores them.
pub(crate) fn memory_file_system(files: &MemoryFiles) -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    let is_file = Arc::clone(files);
    file_system
        .expect_is_file()
        .returning(move |path| Ok(lock(&is_file).contains_key(path)));
    let read = Arc::clone(files);
    file_system.expect_read().returning(move |path| {
        lock(&read)
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    });
    let write = Arc::clone(files);
    file_system.expect_write().returning(move |path, contents| {
        lock(&write).insert(path.to_path_buf(), contents.to_vec());

        Ok(())
    });

    file_system
}

/// Locks `files`, tolerating a panic in another test thread.
pub(crate) fn lock(files: &MemoryFiles) -> MutexGuard<'_, BTreeMap<PathBuf, Vec<u8>>> {
    files.lock().unwrap_or_else(PoisonError::into_inner)
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
