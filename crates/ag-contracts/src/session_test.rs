use crate::session::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};

#[test]
fn response_style_round_trips_persisted_values() {
    // Arrange, Act, Assert
    for response_style in ResponseStyle::ALL {
        assert_eq!(
            response_style.as_str().parse::<ResponseStyle>(),
            Ok(response_style)
        );
        assert_eq!(response_style.to_string(), response_style.as_str());
    }
}

#[test]
fn response_style_rejects_unknown_persisted_value() {
    // Arrange, Act
    let result = "verbose".parse::<ResponseStyle>();

    // Assert
    assert_eq!(result, Err("Unknown response style: verbose".to_string()));
}

#[test]
fn response_style_exposes_stable_labels_and_descriptions() {
    // Arrange, Act
    let values = ResponseStyle::ALL
        .map(|response_style| (response_style.name(), response_style.description()));

    // Assert
    assert_eq!(
        values,
        [
            (
                "Concise",
                "Compact answers with essential results, caveats, and verification."
            ),
            (
                "Balanced",
                "Enough context to understand and verify without exhaustive detail."
            ),
            (
                "Detailed",
                "Thorough decisions, trade-offs, effects, and verification."
            ),
        ]
    );
}

#[test]
fn speed_mode_round_trips_persisted_values() {
    // Arrange, Act, Assert
    for speed_mode in SpeedMode::ALL {
        assert_eq!(speed_mode.as_str().parse::<SpeedMode>(), Ok(speed_mode));
        assert_eq!(speed_mode.to_string(), speed_mode.as_str());
    }
}

#[test]
fn response_style_prompt_instructions_lead_with_the_outcome() {
    // Arrange, Act
    let instructions = ResponseStyle::ALL.map(ResponseStyle::prompt_instruction);

    // Assert
    assert!(
        instructions
            .iter()
            .all(|instruction| instruction.starts_with("Lead with the outcome."))
    );
    assert!(instructions[0].contains("compact"));
    assert!(instructions[1].contains("without exhaustive detail"));
    assert!(instructions[2].contains("trade-offs"));
}

#[test]
fn speed_mode_exposes_stable_labels_and_descriptions() {
    // Arrange, Act
    let values = SpeedMode::ALL.map(|speed_mode| (speed_mode.name(), speed_mode.description()));

    // Assert
    assert_eq!(
        values,
        [
            ("Normal", "Standard response speed and provider pricing."),
            ("Fast", "Faster responses at a higher provider cost."),
        ]
    );
}

#[test]
fn line_change_counts_ignore_file_header_markers() {
    // Arrange
    let diff = "--- a/file.rs\n+++ b/file.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n+extra\n context";

    // Act
    let counts = SessionStats::line_change_counts(diff);

    // Assert
    assert_eq!(counts, (2, 1));
}

#[test]
fn speed_mode_rejects_unknown_persisted_value() {
    // Arrange, Act
    let result = "turbo".parse::<SpeedMode>();

    // Assert
    assert_eq!(result, Err("Unknown speed mode: turbo".to_string()));
}

#[test]
fn should_show_diff_hides_only_known_empty_diffs() {
    // Arrange
    let unknown = SessionStats::default();
    let empty = SessionStats {
        diff_state: SessionDiffState::Empty,
        ..SessionStats::default()
    };
    let present = SessionStats {
        diff_state: SessionDiffState::Present,
        ..SessionStats::default()
    };

    // Act, Assert
    assert!(unknown.should_show_diff());
    assert!(!empty.should_show_diff());
    assert!(present.should_show_diff());
}
