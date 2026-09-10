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
