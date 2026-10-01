use ag_git::DiffFile;
use ag_protocol::{
    FocusedReview, FocusedReviewEvidence, FocusedReviewSeverity, FocusedReviewSide,
    FocusedReviewSuggestion,
};

use crate::app::review_diff::{anchor, chunks, paths, split_hunk, unresolved_files};

fn finding(path: &str, side: FocusedReviewSide, snippet: &str, start: u32) -> FocusedReview {
    FocusedReview {
        project_impact: Vec::new(),
        suggestions: vec![FocusedReviewSuggestion {
            details: "Concrete risk".into(),
            evidence: Some(FocusedReviewEvidence {
                correction: "Restore validation".into(),
                end_line: start,
                existing_code: snippet.into(),
                impact: "Invalid input reaches the operation".into(),
                path: path.into(),
                side,
                start_line: start,
                trigger: "An invalid request".into(),
            }),
            severity: FocusedReviewSeverity::High,
        }],
    }
}

#[test]
fn repairs_wrong_ranges_and_preserves_only_unambiguous_anchors() {
    // Arrange
    let diff = "diff --git a/old.rs b/new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -10,1 +20,1 \
                @@\n-old\n+new\n@@ -30,1 +40,1 @@\n-old\n+other\n";
    let cases = [
        ("new.rs", FocusedReviewSide::New, "new", 999, 20),
        ("old.rs", FocusedReviewSide::Old, "old", 30, 30),
        ("old.rs", FocusedReviewSide::Old, "old", 999, 0),
        ("other.rs", FocusedReviewSide::New, "new", 20, 0),
        ("new.rs", FocusedReviewSide::New, "missing", 20, 0),
    ];

    // Act / Assert
    for (path, side, source, claimed, expected) in cases {
        let mut review = finding(path, side, source, claimed);
        assert_eq!(anchor(&mut review, diff), usize::from(expected == 0));
        let evidence = review.suggestions[0]
            .evidence
            .as_ref()
            .expect("evidence retained");
        assert_eq!(evidence.start_line, expected);
        assert_eq!(evidence.end_line, expected);
    }
    let mut legacy = finding("new.rs", FocusedReviewSide::New, "new", 20);
    legacy.suggestions[0].evidence = None;
    assert_eq!(anchor(&mut legacy, diff), 1);
    assert_eq!(paths(diff).into_iter().collect::<Vec<_>>(), ["new.rs"]);
    assert_eq!(
        paths("diff --git malformed\n"),
        std::collections::BTreeSet::new()
    );
}

#[test]
fn retains_spaced_paths_and_accounts_for_unresolved_file_sections() {
    // Arrange
    let diff = "diff --git a/check script.sh b/check script.sh\nold mode 100644\nnew mode \
                100755\ndiff --git a/image asset.png b/image asset.png\nBinary files a/image \
                asset.png and b/image asset.png differ\ndiff --git malformed\n";

    // Act
    let known = paths(diff);
    let unresolved = unresolved_files(diff);

    // Assert
    assert_eq!(
        known.into_iter().collect::<Vec<_>>(),
        ["check script.sh", "image asset.png"]
    );
    assert_eq!(unresolved, 1);
    assert_eq!(unresolved_files("no diff headers"), 0);
}

#[test]
fn repairs_primary_citations_without_rewriting_supporting_same_file_locations() {
    // Arrange
    let diff = "diff --git a/source.rs b/source.rs\n--- a/source.rs\n+++ b/source.rs\n@@ -10,3 \
                +20,3 @@\n context\n\n-old();\n+new();\n";
    let mut review = finding("source.rs", FocusedReviewSide::New, "new();", 999);
    review.suggestions[0].details =
        "source.rs:80 calls source.rs:999:12; see source.rs:80:4".into();

    // Act
    let unanchored = anchor(&mut review, diff);
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(unanchored, 0);
    assert!(markdown.contains("`source.rs:22`"));
    assert!(markdown.contains("source.rs:80 calls source.rs:22; see source.rs:80:4"));
    assert!(markdown.contains("Supporting references are unverified."));
    assert!(!markdown.contains("999"));
}

#[test]
fn findings_without_typed_evidence_render_unanchored_citations() {
    // Arrange
    let diff = "diff --git a/source.rs b/source.rs\n--- a/source.rs\n+++ b/source.rs\n@@ -1 +1 \
                @@\n-old();\n+new();\n";
    let mut review = finding("source.rs", FocusedReviewSide::New, "new();", 999);
    review.suggestions[0].evidence = None;
    review.suggestions[0].details = "source.rs:999: Unsupported location; caller.rs:80".into();

    // Act
    let unanchored = anchor(&mut review, diff);
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(unanchored, 1);
    assert!(markdown.contains("- [High] (unanchored): source.rs:999: Unsupported location"));
    assert!(markdown.contains("caller.rs:80"));
}

#[test]
fn split_hunks_count_suppressed_blank_context_in_old_and_new_coordinates() {
    // Arrange
    let prefix = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
    for blank in ["\n", "\r\n"] {
        let body = format!("{blank}-old\n+new\n").repeat(30);
        let input = format!("{prefix}@@ -10,60 +20,60 @@\n{body}");

        // Act
        let parts = chunks(&input, 220);
        let old: Vec<_> = parts
            .iter()
            .flat_map(|part| DiffFile::parse(part))
            .flat_map(|file| file.source_ranges("old", true))
            .collect();
        let new: Vec<_> = parts
            .iter()
            .flat_map(|part| DiffFile::parse(part))
            .flat_map(|file| file.source_ranges("new", false))
            .collect();

        // Assert
        assert!(parts.len() > 1);
        assert_eq!(
            old,
            (0..30)
                .map(|index| (11 + 2 * index, 11 + 2 * index))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            new,
            (0..30)
                .map(|index| (21 + 2 * index, 21 + 2 * index))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn packs_whole_files_and_hunks_without_dropping_changed_lines() {
    // Arrange
    let first = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let second = first.replace("a.rs", "b.rs");
    let input = format!("{first}{second}");

    // Act
    let partitioned = chunks(&input, first.len() + 2);

    // Assert
    assert_eq!(partitioned, Vec::from([first.to_string(), second]));
    assert_eq!(chunks(&input, input.len()), Vec::from([input.clone()]));
    let header = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n";
    let body = "+changed\n".repeat(80);
    let input = format!("{header}@@ -0,0 +1,80 @@\n{body}@@ -100 +100 @@\n-old\n+last\n");
    let partitioned = chunks(&input, 360);
    assert!(partitioned.len() > 2);
    assert!(
        partitioned
            .iter()
            .all(|part| part.len() <= 360 && part.starts_with(header))
    );
    assert_eq!(
        partitioned
            .iter()
            .map(|part| part.matches("+changed\n").count())
            .sum::<usize>(),
        80
    );
    assert_eq!(
        partitioned
            .iter()
            .map(|part| part.matches("+last\n").count())
            .sum::<usize>(),
        1
    );
    let ranges: Vec<_> = partitioned
        .iter()
        .flat_map(|part| DiffFile::parse(part)[0].source_ranges("changed", false))
        .collect();
    assert_eq!(ranges.len(), 80);
    assert_eq!(ranges.first(), Some(&(1, 1)));
    assert_eq!(ranges.last(), Some(&(80, 80)));
}

#[test]
fn retains_complete_hunks_before_splitting_a_later_oversized_hunk() {
    // Arrange
    let header = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
    let first_hunk = format!("@@ -0,0 +1,20 @@\n{}", "+before\n".repeat(20));
    let oversized_hunk = format!("@@ -40,80 +50,80 @@\n{}", "-old\n+after\n".repeat(80));
    let input = format!("{header}{first_hunk}{oversized_hunk}");

    // Act
    let partitioned = chunks(&input, 260);

    // Assert
    assert!(partitioned.len() > 2);
    assert_eq!(partitioned.front(), Some(&format!("{header}{first_hunk}")));
    assert!(
        partitioned
            .iter()
            .all(|part| part.len() <= 260 && part.starts_with(header))
    );
    for (snippet, old, start, count) in [
        ("before", false, 1, 20),
        ("old", true, 40, 80),
        ("after", false, 50, 80),
    ] {
        let ranges: Vec<_> = partitioned
            .iter()
            .flat_map(|part| DiffFile::parse(part))
            .flat_map(|file| file.source_ranges(snippet, old))
            .collect();
        assert_eq!(
            ranges,
            (start..start + count)
                .map(|line| (line, line))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn keeps_complete_hunks_together_when_a_file_exceeds_the_limit() {
    // Arrange
    let header = "diff --git a/x b/x\n--- a/x\n+++ b/x\n";
    let hunks = [
        format!("@@ -0,0 +1,20 @@\n{}", "+first\n".repeat(20)),
        format!("@@ -30,0 +30,20 @@\n{}", "+second\n".repeat(20)),
        format!("@@ -60,0 +60,20 @@\n{}", "+third\n".repeat(20)),
    ];
    let input = format!("{header}{}", hunks.join(""));

    // Act
    let partitioned = chunks(&input, 240);

    // Assert
    assert_eq!(partitioned.len(), 3);
    for (part, hunk) in partitioned.iter().zip(hunks) {
        assert_eq!(part, &format!("{header}{hunk}"));
    }
}

#[test]
fn preserves_pathological_input_without_fabricating_source_coordinates() {
    // Arrange
    let inputs = [
        "🦀".repeat(300),
        format!("preamble\ndiff --git a/x b/x\n{}", "x".repeat(300)),
        format!("diff --git a/x b/x\n{}", "metadata\n".repeat(100)),
        format!(
            "diff --git a/x b/x\n{}@@ -1 +1 @@\n+x\n",
            "metadata\n".repeat(100)
        ),
        format!("diff --git a/x b/x\n@@ -1 +1 @@\n+{}\n", "🦀".repeat(300)),
    ];

    // Act / Assert
    for input in inputs {
        let parts = chunks(&input, 300);
        assert_ne!(parts, std::collections::VecDeque::new());
        assert!(parts.iter().all(|part| part.len() <= 300));
        assert_eq!(
            parts
                .iter()
                .map(|part| part.matches('🦀').count())
                .sum::<usize>(),
            input.matches('🦀').count()
        );
    }
    for hunk in ["@@ -1 +1 @@", "@@ malformed\n+x\n"] {
        assert_eq!(
            split_hunk("file\n", hunk, 300)
                .into_iter()
                .collect::<String>(),
            format!("file\n{hunk}")
        );
    }
    assert_eq!(chunks("", 300), std::collections::VecDeque::new());
}
