//! Release notes unit tests with mocked host boundaries.

use std::ffi::OsString;
use std::io;

use serde_json::{Value, json};

use super::{ReleaseNote, run};
use crate::changelog::ChangelogEntry;
use crate::cli::{Host, NotesOptions};
use crate::file_system::MockFileSystem;
use crate::plan::ReleasePlan;
use crate::tag::ReleaseTag;
use crate::workspace::Workspace;
use crate::workspace::test_support::{
    BrokenPipe, demo_metadata, memory_file_system, memory_files, metadata_bytes, metadata_process,
};

/// Changelog of the demo workspace with an entry for its version.
const CHANGELOG: &str = "# Changelog\n\n## [v1.2.3] - 2026-01-02\n\n### Added\n\n- Demo \
                         feature.\n\n## [v1.2.2] - 2026-01-01\n";

/// Release notes of the demo workspace for `v1.2.3` without the changelog
/// entry.
const DEMO_APP_NOTES: &str = r"# demo-app 1.2.3

## Install demo-app 1.2.3

### Install prebuilt binaries via shell script

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/demo-org/demo-app/releases/download/v1.2.3/demo-app-installer.sh | sh
```

### Install prebuilt binaries into your npm project

```sh
npm install demo-app@1.2.3
```

## Download demo-app 1.2.3

|  File  | Platform | Checksum |
|--------|----------|----------|
| [demo-app-aarch64-apple-darwin.tar.gz](https://github.com/demo-org/demo-app/releases/download/v1.2.3/demo-app-aarch64-apple-darwin.tar.gz) | Apple Silicon macOS | [checksum](https://github.com/demo-org/demo-app/releases/download/v1.2.3/demo-app-aarch64-apple-darwin.tar.gz.sha256) |
| [demo-app-x86_64-unknown-linux-gnu.tar.gz](https://github.com/demo-org/demo-app/releases/download/v1.2.3/demo-app-x86_64-unknown-linux-gnu.tar.gz) | x64 Linux | [checksum](https://github.com/demo-org/demo-app/releases/download/v1.2.3/demo-app-x86_64-unknown-linux-gnu.tar.gz.sha256) |

## Verifying GitHub Artifact Attestations

The artifacts in this release have attestations generated with GitHub Artifact Attestations. These can be verified by using the [GitHub CLI](https://cli.github.com/manual/gh_attestation_verify):
```sh
gh attestation verify <file-path of downloaded artifact> --repo demo-org/demo-app
```

You can also download the attestation from [GitHub](https://github.com/demo-org/demo-app/attestations) and verify against that directly:
```sh
gh attestation verify <file-path of downloaded artifact> --bundle <file-path of downloaded attestation>
```
";

/// Runs `notes` for `tag` against the demo workspace and `file_system`, and
/// returns its result with the captured output.
fn run_notes(tag: &str, title: bool, file_system: &MockFileSystem) -> (Result<(), String>, String) {
    let process = metadata_process(&demo_metadata());
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system,
        process: &process,
        source_date_epoch: None,
    };
    let options = NotesOptions {
        tag: tag.to_string(),
        title,
    };
    let mut stdout = Vec::new();
    let result = run(&host, &options, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

/// Returns a file system holding the demo workspace's `CHANGELOG.md`.
fn changelog_file_system(changelog: &[u8]) -> MockFileSystem {
    memory_file_system(&memory_files(&[(
        "/workspace/demo/CHANGELOG.md",
        changelog,
    )]))
}

/// Returns the plan of the demo workspace changed by `edit` for `v1.2.3`.
fn plan(edit: impl FnOnce(&mut Value)) -> ReleasePlan {
    let mut metadata = demo_metadata();
    edit(&mut metadata);
    let workspace = Workspace::from_metadata(&metadata_bytes(&metadata)).expect("demo workspace");
    let tag = ReleaseTag::parse("v1.2.3").expect("release tag");

    ReleasePlan::new(&workspace, Some(&tag)).expect("release plan")
}

#[test]
fn notes_print_the_title_or_the_body_with_the_changelog_entry() {
    // Arrange
    let file_system = changelog_file_system(CHANGELOG.as_bytes());

    // Act
    let title = run_notes("v1.2.3", true, &file_system);
    let body = run_notes("v1.2.3", false, &file_system);

    // Assert
    assert_eq!(title, (Ok(()), "v1.2.3 - 2026-01-02\n".to_string()));
    assert_eq!(body.0, Ok(()));
    let (app, tool) = body
        .1
        .split_once("\n# demo-tool 1.2.3\n")
        .expect("two apps");
    assert_eq!(
        app,
        format!("## Release Notes\n\n### Added\n\n- Demo feature.\n\n{DEMO_APP_NOTES}")
    );
    let expected_tool = DEMO_APP_NOTES
        .replace("demo-app-", "demo-tool-")
        .replace("demo-app 1.2.3", "demo-tool 1.2.3")
        .replace("npm install demo-app@", "npm install @demo-org/demo-tool@");
    assert_eq!(format!("# demo-tool 1.2.3\n{tool}"), expected_tool);
}

#[test]
fn notes_without_a_changelog_entry_use_the_tag_as_title_and_omit_the_release_notes() {
    // Arrange
    let file_systems = [
        changelog_file_system(b"# Changelog\n\n## [v1.2.2] - 2026-01-01\n"),
        changelog_file_system(b""),
        memory_file_system(&memory_files(&[])),
    ];

    for file_system in file_systems {
        // Act
        let title = run_notes("1.2.3", true, &file_system);
        let body = run_notes("1.2.3", false, &file_system);

        // Assert
        assert_eq!(title, (Ok(()), "1.2.3\n".to_string()));
        assert_eq!(body.0, Ok(()));
        assert!(body.1.starts_with("# demo-app 1.2.3\n\n"), "{}", body.1);
    }
}

#[test]
fn notes_report_unreadable_or_non_utf8_changelogs_and_output_failures() {
    // Arrange
    let mut uninspectable = MockFileSystem::new();
    uninspectable
        .expect_is_file()
        .returning(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let non_utf8 = changelog_file_system(&[0xff]);
    let readable = changelog_file_system(CHANGELOG.as_bytes());
    let process = metadata_process(&demo_metadata());
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system: &readable,
        process: &process,
        source_date_epoch: None,
    };
    let options = NotesOptions {
        tag: "v1.2.3".to_string(),
        title: true,
    };

    // Act
    let inspection = run_notes("v1.2.3", false, &uninspectable);
    let decoding = run_notes("v1.2.3", false, &non_utf8);
    let mismatched = run_notes("v9.9.9", false, &readable);
    let output = run(&host, &options, &mut BrokenPipe);

    // Assert
    assert_eq!(
        inspection.0,
        Err("failed to inspect `/workspace/demo/CHANGELOG.md`: permission denied".to_string())
    );
    assert_eq!(
        decoding.0,
        Err(
            "`/workspace/demo/CHANGELOG.md` is not UTF-8: invalid utf-8 sequence of 1 bytes from \
             index 0"
                .to_string()
        )
    );
    assert_eq!(
        mismatched.0,
        Err(
            "release tag `v9.9.9` names version 9.9.9, but the apps have version 1.2.3".to_string()
        )
    );
    assert_eq!(
        output,
        Err("failed to write the release notes: broken pipe".to_string())
    );
}

#[test]
fn body_lists_homebrew_and_binstall_commands_with_the_other_installers() {
    // Arrange
    let plan = plan(|metadata| {
        let release = &mut metadata["metadata"]["release"];
        release["installers"] = json!(["binstall", "homebrew", "npm", "shell"]);
        release["homebrew-tap"] = json!("demo-org/tap");
        let binstall = json!({"binstall": {
            "pkg-url": "{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz",
            "bin-dir": "{ name }-{ target }/{ bin }{ binary-ext }",
            "pkg-fmt": "tgz",
        }});
        metadata["packages"][1]["metadata"] = binstall.clone();
        metadata["packages"][2]["metadata"] = binstall;
    });
    let note = ReleaseNote {
        entry: None,
        plan: &plan,
        tag: "v1.2.3",
    };

    // Act
    let body = note.body();

    // Assert
    let (app, _) = body.split_once("\n## Download").expect("download section");
    let headings = app
        .lines()
        .filter_map(|line| line.strip_prefix("### "))
        .collect::<Vec<_>>();
    assert_eq!(
        headings,
        [
            "Install prebuilt binaries via shell script",
            "Install prebuilt binaries via Homebrew",
            "Install prebuilt binaries into your npm project",
            "Install prebuilt binaries via cargo-binstall",
        ]
    );
    assert!(
        app.contains("\n```sh\nbrew install demo-org/tap/demo-app\n```\n"),
        "{app}"
    );
    assert!(
        app.contains("\n```sh\ncargo binstall demo-app@1.2.3\n```\n"),
        "{app}"
    );
}

#[test]
fn body_omits_install_commands_without_installers_and_lists_macos_before_linux() {
    // Arrange
    let plan = plan(|metadata| {
        let release = &mut metadata["metadata"]["release"];
        release["installers"] = json!([]);
        release["targets"] = json!({
            "aarch64-apple-darwin": "macos-latest",
            "aarch64-unknown-linux-gnu": "ubuntu-22.04-arm",
            "x86_64-apple-darwin": "macos-latest",
            "x86_64-unknown-linux-gnu": "ubuntu-22.04",
        });
    });
    let note = ReleaseNote {
        entry: Some(ChangelogEntry {
            date: None,
            notes: String::new(),
        }),
        plan: &plan,
        tag: "v1.2.3",
    };

    // Act
    let title = note.title();
    let body = note.body();

    // Assert
    assert_eq!(title, "v1.2.3");
    let (app, _) = body
        .split_once("\n## Verifying")
        .expect("attestation section");
    let rows = app
        .lines()
        .filter_map(|line| line.strip_prefix("| [demo-app-"))
        .map(|row| row.split_once(']').map(|(target, _)| target))
        .collect::<Vec<_>>();
    assert!(
        app.starts_with("# demo-app 1.2.3\n\n## Download demo-app 1.2.3\n\n"),
        "{app}"
    );
    assert_eq!(
        rows,
        [
            Some("aarch64-apple-darwin.tar.gz"),
            Some("x86_64-apple-darwin.tar.gz"),
            Some("aarch64-unknown-linux-gnu.tar.gz"),
            Some("x86_64-unknown-linux-gnu.tar.gz"),
        ]
    );
    assert!(app.contains("| Intel macOS |"), "{app}");
}
