//! Public CLI coverage for instruction validation without modifying Git state.

#![cfg(unix)]

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output};
use std::{env, fs, io};

use tempfile::{TempDir, tempdir};

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let fixture = Self {
            directory: tempdir()?,
        };
        fs::create_dir(fixture.root().join("bin"))?;
        fs::create_dir_all(fixture.root().join("skills/new"))?;
        fs::write(
            fixture.root().join("bin/git"),
            "#!/bin/sh\ntest \"$GIT_OPTIONAL_LOCKS\" = '0' || exit 3\ntest \"$*\" = 'ls-files \
             --cached --others --exclude-standard -z' || exit 2\nprintf \
             'AGENTS.md\\000AGENTS.md\\000skills/new/SKILL.md\\000skills/deleted/SKILL.md\\000'\n",
        )?;
        fs::set_permissions(
            fixture.root().join("bin/git"),
            fs::Permissions::from_mode(0o755),
        )?;
        fs::write(
            fixture.root().join("AGENTS.md"),
            "# Instructions\n`prek run check-instructions`\n",
        )?;
        fs::write(
            fixture.root().join("skills/new/SKILL.md"),
            "# Skill\n[Guide](../../AGENTS.md)\n",
        )?;
        fs::write(
            fixture.root().join(".pre-commit-config.yaml"),
            "repos:\n- repo: local\n  hooks:\n  - id: check-instructions\n",
        )?;
        for name in ["CLAUDE.md", "GEMINI.md"] {
            symlink("AGENTS.md", fixture.root().join(name))?;
        }

        Ok(fixture)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn run(&self) -> io::Result<Output> {
        let mut paths = vec![self.root().join("bin")];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));

        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .arg("check-instructions")
            .current_dir(self.root())
            .env("PATH", env::join_paths(paths).map_err(io::Error::other)?)
            .env("GIT_OPTIONAL_LOCKS", "1")
            .output()
    }
}

#[test]
fn cli_reports_success_and_source_locations_for_broken_untracked_instructions() {
    // Arrange
    let fixture = Fixture::new().expect("instruction fixture");

    // Act
    let valid = fixture.run().expect("valid instruction check");
    fs::write(
        fixture.root().join("skills/new/SKILL.md"),
        "# Skill\nRead `docs/missing.md`.\n",
    )
    .expect("broken guide");
    let invalid = fixture.run().expect("invalid instruction check");

    // Assert
    assert!(valid.status.success(), "{valid:?}");
    assert!(
        String::from_utf8_lossy(&valid.stdout).contains("Instruction integrity passed (2 files).")
    );
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stdout)
            .contains("skills/new/SKILL.md:2: missing path docs/missing.md")
    );
}

#[test]
fn cli_fails_for_missing_aliases_and_unreadable_catalogs() {
    // Arrange
    let fixture = Fixture::new().expect("instruction fixture");
    fs::remove_file(fixture.root().join("CLAUDE.md")).expect("remove alias");

    // Act
    let alias = fixture.run().expect("alias check");
    fs::remove_file(fixture.root().join(".pre-commit-config.yaml")).expect("remove catalog");
    let catalog = fixture.run().expect("catalog check");

    // Assert
    assert!(!alias.status.success());
    assert!(
        String::from_utf8_lossy(&alias.stdout)
            .contains("CLAUDE.md: must be a symlink to AGENTS.md")
    );
    assert!(!catalog.status.success());
    assert!(
        String::from_utf8_lossy(&catalog.stdout)
            .contains("Failed to read ./.pre-commit-config.yaml:")
    );
}

#[test]
fn cli_validates_titled_and_angle_delimited_link_destinations() {
    // Arrange
    let fixture = Fixture::new().expect("instruction fixture");
    let target = fixture.root().join("skills/new/guide notes.md");
    fs::write(&target, "# Guide\n").expect("link target");
    fs::write(
        fixture.root().join("skills/new/SKILL.md"),
        "# Skill\n\n[Guide](<guide notes.md> \"Guide\")\n",
    )
    .expect("guide link");

    // Act
    let valid = fixture.run().expect("valid link check");
    fs::remove_file(target).expect("remove link target");
    let missing = fixture.run().expect("missing link check");

    // Assert
    assert!(valid.status.success(), "{valid:?}");
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stdout)
            .contains("skills/new/SKILL.md:3: invalid local link guide notes.md")
    );
}
