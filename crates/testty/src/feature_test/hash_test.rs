use std::path::Path;

use super::super::{
    CommittedHash, normalize_tempfile_segments, normalized_frame_bytes_for_hash,
    read_committed_hash,
};
use crate::feature::{Redaction, compute_frame_hash, compute_gif_hash, hash_sidecar_path};
use crate::frame::TerminalFrame;
use crate::proof::report::ProofReport;
use crate::vhs::VhsTapeSettings;

#[test]
fn compute_frame_hash_deterministic() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new("hash_test");
    report.add_capture("snap", "Snapshot", &frame);

    // Act
    let hash_a = compute_frame_hash(&report, &[]);
    let hash_b = compute_frame_hash(&report, &[]);

    // Assert
    assert_eq!(hash_a, hash_b);
}

#[test]
fn compute_frame_hash_differs_for_different_content() {
    // Arrange
    let frame_a = TerminalFrame::new(80, 24, b"Hello");
    let frame_b = TerminalFrame::new(80, 24, b"World");

    let mut report_a = ProofReport::new("a");
    report_a.add_capture("snap", "A", &frame_a);

    let mut report_b = ProofReport::new("b");
    report_b.add_capture("snap", "B", &frame_b);

    // Act
    let hash_a = compute_frame_hash(&report_a, &[]);
    let hash_b = compute_frame_hash(&report_b, &[]);

    // Assert
    assert_ne!(hash_a, hash_b);
}

#[test]
fn compute_frame_hash_empty_report() {
    // Arrange
    let report = ProofReport::new("empty");

    // Act
    let hash = compute_frame_hash(&report, &[]);

    // Assert — empty reports use the stable FNV-1a offset basis.
    assert_eq!(hash, 0xcbf2_9ce4_8422_2325);
}

#[test]
fn compute_frame_hash_ignores_redacted_tokens() {
    // Arrange — the same UI showing two different generated hashes.
    let frame_a = TerminalFrame::new(80, 24, b"branch wt/4175e5af");
    let frame_b = TerminalFrame::new(80, 24, b"branch wt/9c0b17ff");

    let mut report_a = ProofReport::new("a");
    report_a.add_capture("snap", "A", &frame_a);

    let mut report_b = ProofReport::new("b");
    report_b.add_capture("snap", "B", &frame_b);

    let redactions = [Redaction::hex_after("wt/", 8, "<hash>")];

    // Act
    let hash_a = compute_frame_hash(&report_a, &redactions);
    let hash_b = compute_frame_hash(&report_b, &redactions);

    // Assert — with the token redacted the two frames hash alike.
    assert_eq!(hash_a, hash_b);
    assert_ne!(
        compute_frame_hash(&report_a, &[]),
        compute_frame_hash(&report_b, &[]),
        "without the redaction the same UI must still hash differently",
    );
}

#[test]
fn compute_gif_hash_includes_every_render_setting() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello");
    let mut report = ProofReport::new("settings_hash");
    report.add_capture("snap", "Snapshot", &frame);
    let settings = VhsTapeSettings::feature_demo();
    let expected_hash = compute_gif_hash(&report, &[], &settings);

    let mut different_width = settings.clone();
    different_width.width += 1;
    let mut different_height = settings.clone();
    different_height.height += 1;
    let mut different_font_size = settings.clone();
    different_font_size.font_size += 1;
    let mut different_theme = settings.clone();
    different_theme.theme.push_str("-variant");
    let mut different_framerate = settings.clone();
    different_framerate.framerate += 1;
    let mut different_padding = settings;
    different_padding.padding += 1;
    let variants = [
        different_width,
        different_height,
        different_font_size,
        different_theme,
        different_framerate,
        different_padding,
    ];

    // Act
    let variant_hashes = variants
        .iter()
        .map(|variant| compute_gif_hash(&report, &[], variant))
        .collect::<Vec<_>>();

    // Assert
    assert!(variant_hashes.iter().all(|hash| *hash != expected_hash));
}

#[test]
fn redaction_replaces_every_matching_token() {
    // Arrange — the worktree path and the branch label both carry the hash.
    let redaction = Redaction::hex_after("wt/", 8, "<hash>");
    let frame_text = "<tmp>/<tempdir>/wt/4175e5af  wt/4175e5af";

    // Act
    let redacted = redaction.apply(frame_text);

    // Assert
    assert_eq!(redacted, "<tmp>/<tempdir>/wt/<hash>  wt/<hash>");
}

#[test]
fn redaction_replaces_a_token_cut_off_by_the_terminal_edge() {
    // Arrange — the footer path runs past the right edge, so the frame
    // keeps only the leading digits of the hash.
    let redaction = Redaction::hex_after("wt/", 8, "<hash>");

    // Act
    let short = redaction.apply("<tmp>/<tempdir>/agentty_root/wt/53a");
    let shorter = redaction.apply("<tmp>/<tempdir>/agentty_root/wt/5");

    // Assert — however many digits survive, the frame reads the same.
    assert_eq!(short, "<tmp>/<tempdir>/agentty_root/wt/<hash>");
    assert_eq!(shorter, short);
}

#[test]
fn redaction_preserves_runs_longer_than_the_rule() {
    // Arrange — an 8-digit rule must not clip a full-length hash, and a
    // non-hex label after the prefix is not a hash at all.
    let redaction = Redaction::hex_after("wt/", 8, "<hash>");
    let frame_text = "wt/4175e5afff  wt/topic";

    // Act
    let redacted = redaction.apply(frame_text);

    // Assert
    assert_eq!(redacted, frame_text);
}

#[test]
fn redaction_with_empty_prefix_is_inert() {
    // Arrange
    let redaction = Redaction::hex_after("", 8, "<hash>");
    let frame_text = "wt/4175e5af";

    // Act
    let redacted = redaction.apply(frame_text);

    // Assert
    assert_eq!(redacted, frame_text);
}

#[test]
fn redaction_literal_replaces_every_occurrence() {
    // Arrange — the header paints the version once per captured frame.
    let redaction = Redaction::literal("Agentty v0.13.0", "Agentty <version>");
    let frame_text = "Agentty v0.13.0 | FYI\nAgentty v0.13.0";

    // Act
    let redacted = redaction.apply(frame_text);

    // Assert
    assert_eq!(redacted, "Agentty <version> | FYI\nAgentty <version>");
}

#[test]
fn redaction_literal_with_empty_needle_is_inert() {
    // Arrange
    let redaction = Redaction::literal("", "<version>");
    let frame_text = "Agentty v0.13.0";

    // Act
    let redacted = redaction.apply(frame_text);

    // Assert
    assert_eq!(redacted, frame_text);
}

#[test]
fn normalized_frame_bytes_for_hash_removes_tempfile_directory_names() {
    // Arrange
    let temp_root = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|_| std::env::temp_dir())
        .to_string_lossy()
        .trim_end_matches('/')
        .to_string();
    let first_frame = format!("{temp_root}/.tmpAlpha123/test-project");
    let second_frame = format!("{temp_root}/.tmpBeta456/test-project");

    // Act
    let first_normalized = normalized_frame_bytes_for_hash(first_frame.as_bytes(), &[]);
    let second_normalized = normalized_frame_bytes_for_hash(second_frame.as_bytes(), &[]);

    // Assert
    assert_eq!(first_normalized, second_normalized);
    assert_eq!(
        String::from_utf8(first_normalized).expect("normalized frame should be utf8"),
        "<tmp>/<tempdir>/test-project",
    );
}

#[test]
fn normalize_tempfile_segments_preserves_non_tempfile_paths() {
    // Arrange
    let frame_text = "<tmp>/stable-project";

    // Act
    let normalized = normalize_tempfile_segments(frame_text);

    // Assert
    assert_eq!(normalized, frame_text);
}

#[test]
fn hash_sidecar_path_uses_dot_prefix_next_to_gif() {
    // Arrange
    let dir = Path::new("/tmp/features");

    // Act
    let sidecar = hash_sidecar_path(dir, "session_creation");

    // Assert
    assert_eq!(sidecar, Path::new("/tmp/features/.session_creation.hash"));
}

#[test]
fn read_committed_hash_returns_missing_for_missing_file() {
    // Arrange
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let missing = dir.path().join(".missing.hash");

    // Act
    let parsed = read_committed_hash(&missing);

    // Assert
    assert_eq!(parsed, CommittedHash::Missing);
}

#[test]
fn read_committed_hash_parses_trimmed_decimal() {
    // Arrange
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let path = dir.path().join(".valid.hash");
    std::fs::write(&path, "  12345\n").expect("write hash");

    // Act
    let parsed = read_committed_hash(&path);

    // Assert
    assert_eq!(parsed, CommittedHash::Value(12345));
}

#[test]
fn read_committed_hash_returns_invalid_for_garbage() {
    // Arrange
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    let path = dir.path().join(".garbage.hash");
    std::fs::write(&path, "not-a-number").expect("write hash");

    // Act
    let parsed = read_committed_hash(&path);

    // Assert
    let CommittedHash::Invalid(err) = parsed else {
        unreachable!("expected invalid hash sidecar, got {parsed:?}");
    };

    assert!(err.contains("failed to parse hash sidecar"));
}
