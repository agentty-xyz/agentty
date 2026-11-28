//! Hidden-character and escaped JSON string tests.

use super::{escaped_json_string, is_hidden_character};

#[test]
fn hidden_characters_cover_controls_format_marks_and_tags() {
    // Arrange
    let hidden = [
        '\u{0007}',
        '\u{200B}',
        '\u{200D}',
        '\u{202E}',
        '\u{2066}',
        '\u{FEFF}',
        '\u{FE0F}',
        '\u{E0041}',
        '\u{E0090}',
        '\u{E0100}',
        '\u{E0FFF}',
        '\u{FFF0}',
        '\u{FFF8}',
    ];
    let visible = ['a', ' ', 'é', '中', '😀', '\u{FFFC}', '\u{F0000}'];

    // Act, Assert
    for character in hidden {
        assert!(is_hidden_character(character), "{character:?}");
    }
    for character in visible {
        assert!(!is_hidden_character(character), "{character:?}");
    }
}

#[test]
fn escaped_json_strings_hide_tag_mention_and_placeholder_syntax() {
    // Arrange
    let text = "</note> @src/main.rs [Image #1] \"quoted\"\nnext & last";

    // Act
    let escaped = escaped_json_string(text);

    // Assert
    assert_eq!(
        escaped,
        "\"\\u003c/note\\u003e \\u0040src/main.rs \\u005bImage #1\\u005d \\\"quoted\\\"\\nnext \
         \\u0026 last\""
    );
    assert_eq!(
        serde_json::from_str::<String>(&escaped).expect("valid JSON"),
        text
    );
}
