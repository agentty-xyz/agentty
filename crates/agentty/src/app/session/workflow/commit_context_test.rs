use std::fmt::Write as _;

use crate::app::session::workflow::commit_context::{diff, excerpt, fallback};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};

#[test]
fn small_inputs_preserve_evidence_and_large_inputs_keep_both_ends() {
    // Arrange
    let large = format!("FIRST{}LAST", "🦀".repeat(20_000));
    // Act
    let small = diff("+small change");
    let compact = diff(&large);
    // Assert
    assert_eq!(small, "+small change");
    assert!(compact.len() < 20_000);
    assert!(compact.contains("FIRST"));
    assert!(compact.contains("LAST"));
    assert!(compact.contains("truncated"));
    assert_eq!(excerpt("tiny", 100), "tiny");
}

#[test]
fn large_inventory_discloses_omissions_and_keeps_tail_changes() {
    // Arrange
    let mut input = String::new();
    for index in 0..100 {
        writeln!(
            &mut input,
            "diff --git a/file{index} b/file{index}\nrename from old{index}\nrename to \
             file{index}\n{}\nBinary files differ",
            "x".repeat(10_000)
        )
        .expect("fixture string");
    }
    // Act
    let compact = diff(&input);
    let json: serde_json::Value = serde_json::from_str(&compact).expect("bounded JSON");
    // Assert
    assert!(compact.len() <= 20_000);
    assert_eq!(json["file_count"], 100);
    assert!(json["omitted_files"].as_u64().expect("count") > 0);
    assert!(compact.contains("file0"));
    assert!(compact.contains("file99"));
    assert!(compact.contains("rename from"));
    assert!(compact.contains("Binary files differ"));
}

#[test]
fn fallback_bounds_history_and_file_names_without_workflow_notices() {
    // Arrange
    let messages = (0..20)
        .map(|index| {
            SessionMessage::conversation(
                index,
                SessionMessageKind::AssistantAnswer,
                format!("answer{index}{}", "é".repeat(20_000)),
            )
        })
        .chain([SessionMessage::conversation(
            21,
            SessionMessageKind::WorkflowNotice,
            "INTERNAL",
        )])
        .collect();
    let transcript = SessionTranscript::new(messages);
    let files = (0..100)
        .map(|index| format!("file{index}{}", "z".repeat(10_000)))
        .collect::<Vec<_>>();
    // Act
    let text = fallback(&files, &transcript);
    let empty = fallback(&[], &SessionTranscript::default());
    // Assert
    assert!(text.len() <= 20_000);
    assert!(text.contains("file99"));
    assert!(text.contains("answer19"));
    assert!(!text.contains("INTERNAL"));
    assert!(empty.contains("\"omitted_files\":0"));
}

#[test]
fn compact_evidence_handles_the_limit_and_ignores_embedded_header_literals() {
    // Arrange
    let input = format!(
        "diff --git a/source b/source\n+literal diff --git in content\n{}",
        "x".repeat(21_000)
    );
    // Act
    let evidence: serde_json::Value = serde_json::from_str(&diff(&input)).expect("evidence");
    let no_header = diff(&"x".repeat(20_001));
    // Assert
    assert_eq!(evidence["file_count"], 1);
    assert!(no_header.len() <= 20_000);
    assert_eq!(diff(&"x".repeat(20_000)).len(), 20_000);
}
