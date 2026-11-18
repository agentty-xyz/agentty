//! Changelog entry scan unit tests.

use semver::Version;

use crate::changelog::ChangelogEntry;

/// Returns the entry for version 1.2.3 in `changelog`.
fn find(changelog: &str) -> Option<ChangelogEntry> {
    ChangelogEntry::find(changelog, &Version::new(1, 2, 3))
}

fn entry(date: Option<&str>, notes: &str) -> ChangelogEntry {
    ChangelogEntry {
        date: date.map(ToString::to_string),
        notes: notes.to_string(),
    }
}

#[test]
fn find_accepts_tag_or_version_labels_with_optional_brackets_and_date() {
    // Arrange
    let cases = [
        ("## [v1.2.3] - 2026-01-02\nNotes\n", Some("2026-01-02")),
        ("## v1.2.3 - 2026-01-02\nNotes\n", Some("2026-01-02")),
        ("## [1.2.3] - 2026-01-02\nNotes\n", Some("2026-01-02")),
        ("## 1.2.3\nNotes\n", None),
        ("## [v1.2.3]\nNotes\n", None),
        ("  ##   [1.2.3] -  Next week  \nNotes\n", Some("Next week")),
    ];

    for (changelog, date) in cases {
        // Act
        let found = find(changelog);

        // Assert
        assert_eq!(found, Some(entry(date, "Notes")), "{changelog:?}");
    }
}

#[test]
fn find_ignores_headings_of_other_releases_levels_and_forms() {
    // Arrange
    let changelogs = [
        "## [v1.2.30] - 2026-01-02\nNotes\n",
        "## [v1.2.3-rc.1] - 2026-01-02\nNotes\n",
        "## v1.2.3 -\nNotes\n",
        "## v1.2.3 2026-01-02\nNotes\n",
        "## Release v1.2.3\nNotes\n",
        "# v1.2.3\nNotes\n",
        "### v1.2.3\nNotes\n",
        "##v1.2.3\nNotes\n",
        "    ## v1.2.3\nNotes\n",
        "```\n## v1.2.3\n```\nNotes\n",
        "",
    ];

    for changelog in changelogs {
        // Act
        let found = find(changelog);

        // Assert
        assert_eq!(found, None, "{changelog:?}");
    }
}

#[test]
fn find_keeps_the_section_verbatim_up_to_the_next_level_one_or_two_heading() {
    // Arrange
    let notes = concat!(
        "### Added\r\n",
        "\n",
        "- `demo-app`: add `--flag`.  \n",
        "    indented code\n",
        "\n",
        "````md\n",
        "```\n",
        "## v0.0.1 inside a fence\n",
        "````\n",
        "~~~ text\n",
        "# heading inside a fence\n",
        "~~~~\n",
        "#### Deeper heading",
    );
    let changelogs = [
        format!("# Changelog\n\n## [Unreleased]\n\n## [v1.2.3]\n\n\n{notes}\n \n## [v1.2.2]\n"),
        format!("## [v1.2.3]\n{notes}\n\n# Archive\n## [v1.2.3] - again\n"),
        format!("## [v1.2.3]\n \n{notes}\n\n"),
    ];

    for changelog in &changelogs {
        // Act
        let found = find(changelog);

        // Assert
        assert_eq!(found, Some(entry(None, notes)), "{changelog:?}");
    }
}

#[test]
fn find_returns_an_empty_entry_and_stops_at_an_unclosed_fence() {
    // Arrange
    let empty = "## v1.2.3 - 2026-01-02\n\n## v1.2.2\n";
    let unclosed = "## v1.2.3\n```sh\n## v1.2.2\n";

    // Act
    let found = [find(empty), find(unclosed)];

    // Assert
    assert_eq!(
        found,
        [
            Some(entry(Some("2026-01-02"), "")),
            Some(entry(None, "```sh\n## v1.2.2"))
        ]
    );
}
