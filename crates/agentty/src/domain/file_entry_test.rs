use super::{FileEntry, basename_match_bonus, filter_entries, fuzzy_score};

#[test]
fn filter_entries_ranks_exact_basename_before_deeper_match() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "crates/ag-git/src/setting/lib.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "crates/agentty/src/app/setting.rs".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "setting");

    // Assert
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].path, "crates/agentty/src/app/setting.rs");
}

#[test]
fn filter_entries_prioritizes_directories_for_trailing_slash() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "src/aaa.rs".to_string(),
        },
        FileEntry {
            is_dir: true,
            path: "src/zzz".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "src/");

    // Assert
    assert_eq!(filtered.len(), 2);
    assert!(filtered[0].is_dir);
    assert_eq!(filtered[0].path, "src/zzz");
    assert_eq!(filtered[1].path, "src/aaa.rs");
    assert!(!filtered[1].is_dir);
}

#[test]
fn filter_entries_returns_all_entries_for_empty_query() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "a.txt".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "b.txt".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "");

    // Assert
    assert_eq!(filtered.len(), 2);
}

#[test]
fn fuzzy_score_rejects_query_characters_in_the_wrong_order() {
    // Arrange & Act
    let score = fuzzy_score("abc.txt", &['c', 'b'], "cb", false);

    // Assert
    assert!(score.is_none());
}

#[test]
fn fuzzy_score_stops_after_matching_lowercase_expansion() {
    // Arrange
    let query_chars = ['i'];

    // Act
    let score = fuzzy_score("İ", &query_chars, "i", false);

    // Assert
    assert!(score.is_some());
}

#[test]
fn basename_match_bonus_scores_contains_match() {
    // Arrange & Act
    let score = basename_match_bonus("src/my_setting_helper.rs", "setting");

    // Assert
    assert_eq!(score, 30);
}

#[test]
fn test_filter_entries_case_insensitive() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "src/Main.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "main");

    // Assert
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path, "src/Main.rs");
}

#[test]
fn test_filter_entries_no_match() {
    // Arrange
    let entries = vec![FileEntry {
        is_dir: false,
        path: "hello.txt".to_string(),
    }];

    // Act
    let filtered = filter_entries(&entries, "xyz");

    // Assert
    assert_eq!(filtered, [] as [&FileEntry; 0]);
}

#[test]
fn test_filter_entries_fuzzy_match() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/model.rs".to_string(),
        },
    ];

    // Act — "smr" matches "src/main.rs" (s...m...r) and "src/model.rs"
    // (s...m...r)
    let filtered = filter_entries(&entries, "smr");

    // Assert
    assert_eq!(filtered.len(), 2);
}

#[test]
fn test_filter_entries_fuzzy_ranks_consecutive_higher() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "src/xmxaxixn.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
    ];

    // Act — "main" is consecutive in "src/main.rs" but scattered in the
    // other
    let filtered = filter_entries(&entries, "main");

    // Assert — consecutive match ranked first
    assert_eq!(filtered[0].path, "src/main.rs");
}

#[test]
fn test_filter_entries_exact_basename_prefers_shallower_path() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: ".codex/AGENTS.md".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "docs/AGENTS.md".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "AGENTS.md".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "agents.md");

    // Assert
    assert_eq!(filtered.len(), 3);
    assert_eq!(filtered[0].path, "AGENTS.md");
}

#[test]
fn test_filter_entries_fuzzy_ranks_segment_start_higher() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "docs/domain.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/db.rs".to_string(),
        },
    ];

    // Act — "d" matches segment start in "src/db.rs" (after /) and mid-word
    // in "docs"
    let filtered = filter_entries(&entries, "d");

    // Assert — both match, segment-start bonus means "docs" and "db" both
    // have it
    assert_eq!(filtered.len(), 2);
}

#[test]
fn test_filter_entries_fuzzy_no_match_wrong_order() {
    // Arrange
    let entries = vec![FileEntry {
        is_dir: false,
        path: "abc.txt".to_string(),
    }];

    // Act — "cb" requires c before b, but in "abc" b comes before c
    let filtered = filter_entries(&entries, "cb");

    // Assert
    assert_eq!(filtered, [] as [&FileEntry; 0]);
}

#[test]
fn test_filter_entries_matches_path_segments() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: false,
            path: "src/app/session.rs".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "tests/unit.rs".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "app/session");

    // Assert
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path, "src/app/session.rs");
}

#[test]
fn test_filter_entries_trailing_slash_matches_exact_directory() {
    // Arrange
    let entries = vec![
        FileEntry {
            is_dir: true,
            path: "src".to_string(),
        },
        FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        },
    ];

    // Act
    let filtered = filter_entries(&entries, "src/");

    // Assert
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].path, "src");
    assert!(filtered[0].is_dir);
    assert_eq!(filtered[1].path, "src/main.rs");
    assert!(!filtered[1].is_dir);
}
