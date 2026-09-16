use std::collections::BTreeSet;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(windows)]
use std::os::windows::fs::symlink_file as symlink;
use std::path::{Path, PathBuf};
use std::{fs, io};

use mockall::predicate::eq;
use tempfile::{TempDir, tempdir};

use crate::check_instruction::{Entry, Host, InstructionCheck, MockHost, RealHost, run};

const CATALOG: &str = "repos:\n- repo: local\n  hooks:\n  - id: check\n";

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            directory: tempdir().expect("instruction fixture"),
        };
        fixture.write("AGENTS.md", "# Instructions\n");
        fixture.write(".pre-commit-config.yaml", CATALOG);
        fixture.aliases("");

        fixture
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.root().join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
        fs::write(path, text).expect("fixture file");
    }

    fn aliases(&self, directory: &str) {
        for name in ["CLAUDE.md", "GEMINI.md"] {
            symlink("AGENTS.md", self.root().join(directory).join(name))
                .expect("instruction alias");
        }
    }

    fn host(&self, documents: &[&str]) -> MockHost {
        let mut host = MockHost::new();
        let paths: Vec<_> = documents.iter().map(PathBuf::from).collect();
        host.expect_inventory()
            .with(eq(self.root().to_owned()))
            .once()
            .return_once(|_| Ok(paths));
        host.expect_read()
            .returning(|path| fs::read_to_string(path));
        host.expect_inspect().returning(|path| {
            RealHost {
                git: PathBuf::from("git"),
            }
            .inspect(path)
        });
        host.expect_canonicalize()
            .returning(|path| fs::canonicalize(path));

        host
    }

    fn check(&self, documents: &[&str]) -> Result<usize, String> {
        let host = self.host(documents);

        InstructionCheck::new(&host)
            .expect("patterns")
            .run(self.root())
    }
}

#[test]
fn valid_paths_links_and_local_and_remote_hooks_pass() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write("docs/guide.md", "# Guide\n");
    fixture.write("skills/example/references/usage notes.md", "# Notes\n");
    fixture.write(
        ".pre-commit-config.yaml",
        &format!("{CATALOG}- repo: https://example.test/hooks\n  hooks:\n  - id: remote-check\n"),
    );
    fixture.write("skills/example/SKILL.md", "# Example\n\
`docs/guide.md` and `docs/` and `.pre-commit-config.yaml`.\n\
[Notes](references/usage%20notes.md?view=1#details)\n\
[External](https://example.test/absent) [Remote](//example.test/path)\n\
[Email](mailto:contributor@example.test) [Section](#example) [Query](?view=1)\n\
`prek run check --all-files` and `prek run remote-check`\n\
```sh\nprek run check --all-files\nprek run --all-files\n```\n");

    // Act
    let result = fixture.check(&["AGENTS.md", "skills/example/SKILL.md"]);

    // Assert
    assert_eq!(result, Ok(2));
}

#[test]
fn markdown_destinations_with_titles_spaces_and_parentheses_are_checked() {
    // Arrange
    for (markdown, destination) in [
        (r#"[Guide](docs/guide.md "Guide")"#, "docs/guide.md"),
        ("[Guide](docs/guide.md 'Guide')", "docs/guide.md"),
        ("[Guide](docs/guide.md (Guide))", "docs/guide.md"),
        ("[Guide](<docs/guide file.md>)", "docs/guide file.md"),
        (
            r#"[Guide](<docs/guide file.md> "Guide")"#,
            "docs/guide file.md",
        ),
        ("[Guide](docs/guide(part(1)).md)", "docs/guide(part(1)).md"),
        (r"[Guide](docs/guide\(part\).md)", "docs/guide(part).md"),
        (
            "[Guide][usage]\n\n[usage]: <docs/guide file.md> 'Guide'",
            "docs/guide file.md",
        ),
        (
            r#"![Guide](<docs/guide file.md> "Guide")"#,
            "docs/guide file.md",
        ),
        ("[Guide](\n docs/guide.md\n \"Guide\"\n)", "docs/guide.md"),
    ] {
        let fixture = Fixture::new();
        fixture.write(destination, "# Guide\n");
        fixture.write("AGENTS.md", &format!("# Instructions\n\n{markdown}\n"));

        // Act
        let valid = fixture.check(&["AGENTS.md"]);
        fs::remove_file(fixture.root().join(destination)).expect("remove link target");
        let missing = fixture.check(&["AGENTS.md"]);

        // Assert
        assert_eq!(valid, Ok(1), "{markdown}");
        assert!(
            missing
                .expect_err(markdown)
                .starts_with("AGENTS.md:3: invalid local link "),
            "{markdown}"
        );
    }
}

#[test]
fn external_links_and_literal_markdown_examples_are_not_local_targets() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write(
        "AGENTS.md",
        "# Instructions\n\n[External](https://example.test/absent \
         \"External\")\n[Remote](<//example.test/absent path> \
         'Remote')\n[Email](mailto:author@example.test \"Email\") [Anchor](#instructions \
         \"Anchor\")\n\n`[Example](docs/missing-inline.md)`\n\n```md\n[Example](docs/\
         missing-fenced.md)\n```\n",
    );

    // Act
    let result = fixture.check(&["AGENTS.md"]);

    // Assert
    assert_eq!(result, Ok(1));
}

#[test]
fn missing_references_are_aggregated_with_source_lines() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write(
        "AGENTS.md",
        "# Instructions\n\nRead `docs/removed.md`.\n```sh\nprek run removed-check \
         --all-files\n```\n",
    );
    fixture.write("CONTRIBUTING.md", "[Architecture](docs/removed.md)\n");

    // Act
    let result = fixture.check(&["CONTRIBUTING.md", "AGENTS.md"]);

    // Assert
    assert_eq!(
        result,
        Err(
            "AGENTS.md:3: missing path docs/removed.md\nAGENTS.md:5: unknown hook \
             removed-check\nCONTRIBUTING.md:1: invalid local link docs/removed.md"
                .to_owned()
        )
    );
}

#[test]
fn placeholders_identifiers_and_multiple_backticks_are_ignored() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write(
        "AGENTS.md",
        "# Instructions\n`crates/<name>/src/lib.rs` `crates/example/{module}.rs` \
         `crates/*/Cargo.toml`\n`crates/ag-store/migrations/NNN_description.sql` \
         `std::fs::read()` `linux/arm64`\n``docs/not-a-reference.md``\n",
    );

    // Act
    let result = fixture.check(&["AGENTS.md"]);

    // Assert
    assert_eq!(result, Ok(1));
}

#[test]
fn local_links_cannot_escape_through_parent_components_or_symlinks() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write(
        "skills/example/SKILL.md",
        "[Outside](../../AGENTS.md)\n[Alias](alias.md)\n",
    );
    symlink(
        fixture.root().join("AGENTS.md"),
        fixture.root().join("skills/example/alias.md"),
    )
    .expect("escaping alias");
    let mut host = MockHost::new();
    host.expect_canonicalize()
        .returning(|path| fs::canonicalize(path));
    let check = InstructionCheck::new(&host).expect("patterns");
    let root = fs::canonicalize(fixture.root().join("skills")).expect("nested root");
    let text = fs::read_to_string(root.join("example/SKILL.md")).expect("nested guide");
    let mut errors = Vec::new();

    // Act
    check
        .check_text(
            &root,
            Path::new("example/SKILL.md"),
            &text,
            &BTreeSet::new(),
            &mut errors,
        )
        .expect("read links");

    // Assert
    assert_eq!(errors.len(), 2);
    assert!(
        errors
            .iter()
            .all(|error| error.contains("invalid local link"))
    );
}

#[test]
fn missing_copied_and_wrong_aliases_fail_in_their_own_directory() {
    // Arrange
    for alias_kind in [
        None,
        Some(Entry::File),
        Some(Entry::Symlink(PathBuf::from("README.md"))),
    ] {
        let mut host = MockHost::new();
        host.expect_inspect()
            .with(eq(Path::new("repo/skills/example/CLAUDE.md")))
            .once()
            .return_once(|_| Ok(alias_kind));
        host.expect_inspect()
            .with(eq(Path::new("repo/skills/example/GEMINI.md")))
            .once()
            .return_once(|_| Ok(Some(Entry::Symlink(PathBuf::from("AGENTS.md")))));
        let check = InstructionCheck::new(&host).expect("patterns");
        let mut errors = Vec::new();

        // Act
        check
            .check_aliases(
                Path::new("repo"),
                Path::new("skills/example/AGENTS.md"),
                &mut errors,
            )
            .expect("inspect aliases");

        // Assert
        assert_eq!(
            errors,
            ["skills/example/CLAUDE.md: must be a symlink to AGENTS.md"]
        );
    }
}

#[test]
fn inventory_deduplicates_and_keeps_untracked_guides_but_skips_aliases_and_deletions() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write("skills/new/SKILL.md", "# New skill\n");
    fixture.write("skills/new/references/details.md", "# Details\n");
    symlink("SKILL.md", fixture.root().join("skills/new/CLAUDE.md")).expect("skill alias");
    fixture.write("crates/example/AGENTS.md", "# Local\n");
    fixture.aliases("crates/example");
    fixture.write("src/unrelated.md", "# Unrelated\n");
    let names = [
        "AGENTS.md",
        "AGENTS.md",
        "CLAUDE.md",
        "skills/new/SKILL.md",
        "skills/new/references/details.md",
        "skills/new/CLAUDE.md",
        "crates/example/AGENTS.md",
        "skills/deleted/SKILL.md",
        "src/unrelated.md",
        "skills/not-markdown.rs",
    ];

    // Act
    let result = fixture.check(&names);

    // Assert
    assert_eq!(result, Ok(4));
}

#[test]
fn missing_root_guide_cannot_pass_as_an_empty_inventory() {
    // Arrange
    let fixture = Fixture::new();
    fs::remove_file(fixture.root().join("AGENTS.md")).expect("remove root guide");

    // Act
    let result = fixture.check(&["AGENTS.md"]);

    // Assert
    assert_eq!(
        result,
        Err("AGENTS.md: missing root instruction guide".to_owned())
    );
}

#[test]
fn malformed_or_incomplete_catalog_fails() {
    // Arrange
    let fixture = Fixture::new();
    for content in [
        "[unterminated",
        "{}",
        "null",
        "repos: [{}]",
        "repos: [{hooks: [{}]}]",
    ] {
        fixture.write(".pre-commit-config.yaml", content);
        let mut host = MockHost::new();
        host.expect_read()
            .once()
            .return_once(|path| fs::read_to_string(path));
        let check = InstructionCheck::new(&host).expect("patterns");

        // Act
        let result = check.run(fixture.root());

        // Assert
        assert!(
            result
                .expect_err("invalid catalog")
                .starts_with("Invalid hook catalog:")
        );
    }
}

#[test]
fn unreadable_catalog_and_document_fail() {
    // Arrange
    for fail_catalog in [true, false] {
        let mut host = MockHost::new();
        host.expect_read().returning(move |path| {
            if !fail_catalog && path.ends_with(".pre-commit-config.yaml") {
                Ok(CATALOG.to_owned())
            } else {
                Err(io::Error::other("read failed"))
            }
        });
        host.expect_inventory()
            .returning(|_| Ok(vec![PathBuf::from("AGENTS.md")]));
        host.expect_inspect().returning(|_| Ok(Some(Entry::File)));
        host.expect_canonicalize()
            .returning(|path| Ok(path.to_owned()));
        let check = InstructionCheck::new(&host).expect("patterns");

        // Act
        let result = check.run(Path::new("repo"));

        // Assert
        let name = if fail_catalog {
            ".pre-commit-config.yaml"
        } else {
            "AGENTS.md"
        };
        assert_eq!(
            result,
            Err(format!("Failed to read repo/{name}: read failed"))
        );
    }
}

#[test]
fn git_failure_and_unreadable_root_do_not_become_empty_successes() {
    // Arrange
    for fail_git in [true, false] {
        let mut host = MockHost::new();
        host.expect_read().returning(|_| Ok(CATALOG.to_owned()));
        host.expect_inventory().returning(move |_| {
            if fail_git {
                Err("git unavailable".to_owned())
            } else {
                Ok(Vec::new())
            }
        });
        host.expect_canonicalize()
            .returning(|_| Err(io::Error::other("root unreadable")));
        let check = InstructionCheck::new(&host).expect("patterns");

        // Act
        let result = check.run(Path::new("repo"));

        // Assert
        let message = if fail_git {
            "git unavailable"
        } else {
            "Failed to resolve repository root: root unreadable"
        };
        assert_eq!(result, Err(message.to_owned()));
    }
}

#[test]
fn metadata_and_reference_permission_failures_are_observable() {
    // Arrange
    let mut host = MockHost::new();
    host.expect_inspect()
        .returning(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    host.expect_canonicalize()
        .returning(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied")));
    let check = InstructionCheck::new(&host).expect("patterns");

    // Act
    let inspected = check.inspect(Path::new("repo/AGENTS.md"));
    let resolved = check.resolve(Path::new("repo/docs/guide.md"));

    // Assert
    assert_eq!(
        inspected,
        Err("Failed to inspect repo/AGENTS.md: denied".to_owned())
    );
    assert_eq!(
        resolved,
        Err("Failed to resolve repo/docs/guide.md: denied".to_owned())
    );
}

#[test]
fn invalid_regex_is_an_error() {
    // Arrange / Act
    let result = InstructionCheck::pattern("[");

    // Assert
    assert!(
        result
            .expect_err("invalid pattern")
            .starts_with("Invalid instruction pattern:")
    );
}

#[test]
fn real_adapter_distinguishes_entries_and_reports_io_failures() {
    // Arrange
    let fixture = Fixture::new();
    let host = RealHost {
        git: PathBuf::from("git"),
    };

    // Act
    let contents = host.read(&fixture.root().join("AGENTS.md"));
    let file = host.inspect(&fixture.root().join("AGENTS.md"));
    let directory = host.inspect(fixture.root());
    let alias = host.inspect(&fixture.root().join("CLAUDE.md"));
    let missing = host.inspect(&fixture.root().join("absent"));
    let not_directory = host.inspect(&fixture.root().join("AGENTS.md/child"));
    let invalid = host.inspect(&fixture.root().join("x".repeat(300)));
    let root = host.canonicalize(fixture.root());
    let missing_read = host.read(&fixture.root().join("absent"));

    // Assert
    assert_eq!(contents.expect("guide"), "# Instructions\n");
    assert_eq!(file.expect("file metadata"), Some(Entry::File));
    assert_eq!(directory.expect("directory metadata"), Some(Entry::Other));
    assert_eq!(
        alias.expect("alias metadata"),
        Some(Entry::Symlink(PathBuf::from("AGENTS.md")))
    );
    assert_eq!(missing.expect("missing metadata"), None);
    assert_eq!(not_directory.expect("file child metadata"), None);
    assert!(invalid.is_err());
    assert!(root.is_ok());
    assert!(missing_read.is_err());
}

#[test]
fn real_git_inventory_is_read_only_and_reports_execution_failures() {
    // Arrange
    let directory = tempdir().expect("Git fixture directory");
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let unavailable = RealHost {
        git: directory.path().join("missing-git"),
    };

    // Act
    let listing = host.inventory(Path::new(env!("CARGO_MANIFEST_DIR")));
    let not_executable = unavailable.inventory(directory.path());

    // Assert
    assert!(
        listing
            .expect("real Git inventory")
            .contains(&PathBuf::from("src/main.rs"))
    );
    assert!(
        not_executable
            .expect_err("missing Git")
            .starts_with("Failed to run git ls-files:")
    );
}

#[cfg(unix)]
#[test]
fn failed_and_non_utf8_git_inventory_cannot_pass_as_empty_results() {
    // Arrange
    let directory = tempdir().expect("Git stub directory");
    let git = directory.path().join("git");
    for (script, message) in [
        (
            "#!/bin/sh\nprintf '\\377\\000'\n",
            "Invalid UTF-8 in git ls-files:",
        ),
        (
            "#!/bin/sh\necho 'inventory failed' >&2\nexit 7\n",
            "git ls-files failed: inventory failed",
        ),
    ] {
        fs::write(&git, script).expect("Git stub");
        fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("executable stub");
        let host = RealHost { git: git.clone() };

        // Act
        let result = host.inventory(directory.path());

        // Assert
        assert!(result.expect_err("invalid inventory").starts_with(message));
    }
}

#[test]
fn production_composition_requires_the_workspace_root() {
    // Arrange
    assert!(!Path::new(".pre-commit-config.yaml").exists());

    // Act
    let result = run();

    // Assert
    assert!(
        result
            .expect_err("missing catalog")
            .starts_with("Failed to read ./.pre-commit-config.yaml:")
    );
}
