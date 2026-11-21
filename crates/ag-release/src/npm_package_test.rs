//! npm package rendering unit tests. Installing and running the package is
//! covered by the command-line tests in `tests/cli/npm_package.rs`.

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use serde_json::{Value, json};

use crate::npm_package::NpmPackage;
use crate::plan::ReleasePlan;
use crate::workspace::Workspace;
use crate::workspace::test_support::{demo_metadata, metadata_bytes};

/// Returns the demo release plan for `metadata`.
fn plan(metadata: &Value) -> ReleasePlan {
    let workspace = Workspace::from_metadata(&metadata_bytes(metadata)).expect("demo workspace");

    ReleasePlan::new(&workspace, None).expect("release plan")
}

/// Returns a stand-in digest for every archive of `plan`: its target triple
/// written twice.
fn digests(plan: &ReleasePlan) -> BTreeMap<String, String> {
    plan.apps
        .iter()
        .flat_map(|app| &app.archives)
        .map(|archive| (archive.name.clone(), archive.target.triple().repeat(2)))
        .collect()
}

/// Returns the `index`th app's package for the release `v1.2.3` of `plan`.
fn package<'release>(
    plan: &'release ReleasePlan,
    index: usize,
    sha256: &'release BTreeMap<String, String>,
) -> NpmPackage<'release> {
    let app = &plan.apps[index];

    NpmPackage {
        app,
        documents: vec![("README.md", b"# demo\n".to_vec())],
        name: app.npm_package.as_deref().expect("npm package name"),
        plan,
        sha256,
        tag: "v1.2.3",
    }
}

/// Returns every entry of the `.tar.gz` `tarball` as path, mode, and
/// contents.
fn entries(tarball: &[u8]) -> Vec<(String, u32, String)> {
    let mut archive = tar::Archive::new(GzDecoder::new(tarball));

    archive
        .entries()
        .expect("tar entries")
        .map(|entry| {
            let mut entry = entry.expect("tar entry");
            let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            let mode = entry.header().mode().expect("entry mode");
            let mut contents = String::new();
            entry.read_to_string(&mut contents).expect("UTF-8 entry");

            (path, mode, contents)
        })
        .collect()
}

/// Returns the contents of the file `path` in `entries`.
fn file<'entries>(entries: &'entries [(String, u32, String)], path: &str) -> &'entries str {
    entries
        .iter()
        .find(|(entry, _, _)| entry == path)
        .map(|(_, _, contents)| contents.as_str())
        .expect("file in the package")
}

#[test]
fn to_tar_gz_writes_a_deterministic_package() {
    // Arrange
    let plan = plan(&demo_metadata());
    let sha256 = digests(&plan);
    let package = package(&plan, 0, &sha256);

    // Act
    let tarball = package.to_tar_gz(1_700_000_000).expect("package tarball");
    let again = package.to_tar_gz(1_700_000_000).expect("package tarball");

    // Assert
    assert_eq!(tarball, again);
    let entries = entries(&tarball);
    let layout = entries
        .iter()
        .map(|(path, mode, _)| (path.as_str(), *mode))
        .collect::<Vec<_>>();
    assert_eq!(
        layout,
        [
            ("package/README.md", 0o644),
            ("package/binary.js", 0o644),
            ("package/install.js", 0o644),
            ("package/package.json", 0o644),
            ("package/run-demo-app.js", 0o755),
            ("package/run-demo-helper.js", 0o755),
        ]
    );
    assert_eq!(file(&entries, "package/README.md"), "# demo\n");
    let manifest = file(&entries, "package/package.json");
    assert!(manifest.ends_with("}\n"), "{manifest}");
    let manifest: Value = serde_json::from_str(manifest).expect("package.json");
    assert_eq!(
        manifest,
        json!({
            "name": "demo-app",
            "version": "1.2.3",
            "description": "Demo package",
            "license": "Apache-2.0",
            "repository": {"type": "git", "url": "git+https://github.com/demo-org/demo-app.git"},
            "contributors": ["Demo Author <demo@example.com>"],
            "bin": {"demo-app": "run-demo-app.js", "demo-helper": "run-demo-helper.js"},
            "scripts": {"postinstall": "node ./install.js"},
            "engines": {"node": ">=18"},
            "os": ["darwin", "linux"],
            "cpu": ["arm64", "x64"],
            "preferUnplugged": true,
        })
    );
    assert_eq!(
        file(&entries, "package/install.js"),
        include_str!("template/npm_install.js")
    );
    assert_eq!(
        file(&entries, "package/run-demo-helper.js"),
        "#!/usr/bin/env node\n\"use strict\";\n// Runs `demo-helper`, installing the binaries \
         first when they are missing.\n\nrequire(\"./binary.js\").run(\"demo-helper\");\n"
    );
}

#[test]
fn binary_script_bakes_in_every_release_value() {
    // Arrange
    let plan = plan(&demo_metadata());
    let sha256 = digests(&plan);
    let package = package(&plan, 1, &sha256);

    // Act
    let entries = entries(&package.to_tar_gz(0).expect("package tarball"));

    // Assert
    let script = file(&entries, "package/binary.js");
    let version = env!("CARGO_PKG_VERSION");
    assert!(script.starts_with(&format!(
        "\"use strict\";\n// Installs and runs the demo-tool 1.2.3 binaries from \
         https://github.com/demo-org/demo-app.\n// Generated by ag-release {version}.\n"
    )));
    assert!(script.contains("// Set DEMO_TOOL_DOWNLOAD_URL to download"));
    assert!(script.ends_with("\nmodule.exports = { install, run };\n"));
    let (_, release) = script
        .split_once("const RELEASE = ")
        .expect("release values");
    let (release, _) = release.split_once(";\n").expect("release values");
    let release: Value = serde_json::from_str(release).expect("release JSON");
    let target = |target: &str, platform: &str, arch: &str, libc: &str| {
        json!({
            "arch": arch,
            "archive": format!("demo-tool-{target}.tar.gz"),
            "directory": format!("demo-tool-{target}"),
            "libc": libc,
            "platform": platform,
            "sha256": target.repeat(2),
            "target": target,
        })
    };
    assert_eq!(
        release,
        json!({
            "app": "demo-tool",
            "binaries": ["demo-tool"],
            "downloadUrl": "https://github.com/demo-org/demo-app/releases/download/v1.2.3",
            "downloadUrlVariable": "DEMO_TOOL_DOWNLOAD_URL",
            "minGlibc": "2.35",
            "targets": [
                target("aarch64-apple-darwin", "darwin", "arm64", "none"),
                target("x86_64-unknown-linux-gnu", "linux", "x64", "gnu"),
            ],
            "version": "1.2.3",
        })
    );
    let manifest: Value =
        serde_json::from_str(file(&entries, "package/package.json")).expect("package.json");
    assert_eq!(manifest["name"], "@demo-org/demo-tool");
}

#[test]
fn package_json_omits_missing_cargo_metadata() {
    // Arrange
    let mut metadata = demo_metadata();
    let release = &mut metadata["metadata"]["release"];
    release["targets"] = json!({"x86_64-unknown-linux-gnu": "ubuntu-22.04"});
    release
        .as_object_mut()
        .expect("release table")
        .remove("min-glibc");
    for package in metadata["packages"].as_array_mut().expect("packages") {
        package["authors"] = json!([]);
        package["description"] = Value::Null;
        package["license"] = Value::Null;
    }
    let plan = plan(&metadata);
    let sha256 = digests(&plan);
    let mut package = package(&plan, 1, &sha256);
    package.documents.clear();

    // Act
    let entries = entries(&package.to_tar_gz(0).expect("package tarball"));

    // Assert
    let manifest: Value =
        serde_json::from_str(file(&entries, "package/package.json")).expect("package.json");
    let keys = manifest
        .as_object()
        .expect("package.json object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "name",
            "version",
            "repository",
            "bin",
            "scripts",
            "engines",
            "os",
            "cpu",
            "preferUnplugged"
        ]
    );
    assert_eq!(manifest["os"], json!(["linux"]));
    assert_eq!(manifest["cpu"], json!(["x64"]));
    assert!(file(&entries, "package/binary.js").contains("\"minGlibc\": null,"));
    assert!(
        !entries
            .iter()
            .any(|(path, _, _)| path == "package/README.md")
    );
}

#[test]
fn to_tar_gz_requires_every_archive_digest() {
    // Arrange
    let plan = plan(&demo_metadata());
    let mut sha256 = digests(&plan);
    sha256.remove("demo-app-aarch64-apple-darwin.tar.gz");
    let package = package(&plan, 0, &sha256);

    // Act
    let result = package.to_tar_gz(0);

    // Assert
    assert_eq!(
        result,
        Err("no SHA-256 is recorded for `demo-app-aarch64-apple-darwin.tar.gz`".to_string())
    );
}
