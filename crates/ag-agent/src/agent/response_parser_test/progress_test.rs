use super::*;

#[test]
fn test_compact_progress_message_from_stream_label_maps_compaction_labels() {
    // Arrange
    let compaction_label = "context_compaction";
    let compression_label = "context-compression";

    // Act
    let compaction_progress = compact_progress_message_from_stream_label(compaction_label);
    let compression_progress = compact_progress_message_from_stream_label(compression_label);

    // Assert
    assert_eq!(compaction_progress, Some("Compacting context".to_string()));
    assert_eq!(compression_progress, Some("Compacting context".to_string()));
}
