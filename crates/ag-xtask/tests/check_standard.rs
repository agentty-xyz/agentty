//! Public CLI coverage for standards manifest validation without modifying
//! Git state.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::{env, fs, io};

use tempfile::{TempDir, tempdir};

/// Repository files the fixture writes and its stub `git ls-files` lists.
const FILES: [(&str, &str); 15] = [
    (
        "standards.toml",
        "version = 4\nfiles = [\"clippy.toml\"]\nskills = []\n\n[toolchain]\nfile = \
         \"rust-toolchain.toml\"\n\n[lints]\nmanifest = \"Cargo.toml\"\n\n[rules]\ndirectory = \
         \"lint/rules/shared\"\ntests = \"lint/rule-tests/shared\"\n\n[[instructions]]\nfile = \
         \"AGENTS.md\"\nsections = [\"Shared\"]\n\n[design]\nguide = \"design/DESIGN.md\"\nfiles \
         = [\"design/tokens.css\"]\ntokens = \"design/tokens.json\"\nassets = \
         [\"design/mark.svg\"]\n\n[protocol]\nschema = \"schema/protocol.json\"\ntypes = \
         \"schema/protocol.ts\"\n",
    ),
    (
        "schema/protocol.json",
        "{ \"x-protocol-version\": 1, \"$ref\": \"#/$defs/Frame\", \"$defs\": { \"Frame\": {} } \
         }\n",
    ),
    (
        "schema/protocol.ts",
        "// Generated from `ag-demo`.\nexport type ProtocolVersion = 1;\n",
    ),
    ("design/DESIGN.md", "# Design\n\nLoad `tokens.css` first.\n"),
    ("design/tokens.css", ":root {\n  --color-bg: #000000;\n}\n"),
    (
        "design/tokens.json",
        "{ \"$extensions\": { \"org.ag-demo.theme\": {} } }\n",
    ),
    (
        "design/mark.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0\"/></svg>\n",
    ),
    ("clippy.toml", "warn-on-all-wildcard-imports = true\n"),
    (
        "rust-toolchain.toml",
        "[toolchain]\nchannel = \"nightly\"\n",
    ),
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.lints.rust]\n",
    ),
    (
        "crates/ag-demo/Cargo.toml",
        "[package]\nname = \"ag-demo\"\n",
    ),
    (
        ".pre-commit-config.yaml",
        "repos:\n- repo: local\n  hooks:\n  - id: check-standards\n",
    ),
    (
        "AGENTS.md",
        "# Instructions\n\n## Shared\n\n- Keep `clippy.toml` strict.\n\n## Local\n\n- Run \
         `ag-demo`.\n",
    ),
    (
        "lint/rules/shared/no-todo.yml",
        "id: no-todo\nlanguage: rust\nrule:\n  pattern: todo!()\n",
    ),
    (
        "lint/rule-tests/shared/no-todo-test.yml",
        "id: no-todo\nvalid:\n  - fn done() {}\n",
    ),
];

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let fixture = Self {
            directory: tempdir()?,
        };
        let mut listing = String::new();
        for (path, text) in FILES {
            fixture.write(path, text)?;
            listing.push_str(path);
            listing.push_str("\\000");
        }
        fixture.write(
            "bin/git",
            &format!(
                "#!/bin/sh\ntest \"$GIT_OPTIONAL_LOCKS\" = '0' || exit 3\ntest \"$*\" = 'ls-files \
                 --cached --others --exclude-standard -z' || exit 2\nprintf '{listing}'\n"
            ),
        )?;
        fs::set_permissions(
            fixture.root().join("bin/git"),
            fs::Permissions::from_mode(0o755),
        )?;

        Ok(fixture)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, path: &str, text: &str) -> io::Result<()> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap_or(self.root()))?;

        fs::write(path, text)
    }

    fn run(&self) -> io::Result<Output> {
        let mut paths = vec![self.root().join("bin")];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));

        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .arg("check-standards")
            .current_dir(self.root())
            .env("PATH", env::join_paths(paths).map_err(io::Error::other)?)
            .env("GIT_OPTIONAL_LOCKS", "1")
            .output()
    }
}

#[test]
fn cli_reports_success_and_the_location_of_unportable_exports() {
    // Arrange
    let fixture = Fixture::new().expect("standards fixture");

    // Act
    let valid = fixture.run().expect("valid standards check");
    fixture
        .write(
            "AGENTS.md",
            "# Instructions\n\n## Shared\n\n- Run `ag-demo` through `prek run check-standards`.\n",
        )
        .expect("unportable guide");
    fixture
        .write("schema/protocol.ts", "export type ProtocolVersion = 2;\n")
        .expect("protocol types out of step");
    let invalid = fixture.run().expect("invalid standards check");

    // Assert
    assert!(valid.status.success(), "{valid:?}");
    assert!(
        String::from_utf8_lossy(&valid.stdout)
            .contains("Standards manifest passed (9 exported documents).")
    );
    assert!(!invalid.status.success());
    let report = String::from_utf8_lossy(&invalid.stdout);
    assert!(
        report.contains("AGENTS.md:5: names workspace crate ag-demo"),
        "{report}"
    );
    assert!(
        report.contains("AGENTS.md:5: names workspace hook check-standards"),
        "{report}"
    );
    assert!(
        report.contains(
            "schema/protocol.ts: ProtocolVersion 2 differs from x-protocol-version 1 in \
             schema/protocol.json"
        ),
        "{report}"
    );
}

#[test]
fn cli_fails_for_missing_manifests_and_exports() {
    // Arrange
    let fixture = Fixture::new().expect("standards fixture");
    fs::remove_file(fixture.root().join("clippy.toml")).expect("remove exported file");

    // Act
    let export = fixture.run().expect("export check");
    fs::remove_file(fixture.root().join("standards.toml")).expect("remove manifest");
    let manifest = fixture.run().expect("manifest check");

    // Assert
    assert!(!export.status.success());
    assert!(
        String::from_utf8_lossy(&export.stdout)
            .contains("standards.toml: missing path clippy.toml")
    );
    assert!(!manifest.status.success());
    assert!(String::from_utf8_lossy(&manifest.stdout).contains("Failed to read ./standards.toml:"));
}
