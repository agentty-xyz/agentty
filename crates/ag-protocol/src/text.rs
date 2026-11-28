//! Neutralizing helpers for untrusted text that the host places in prompts.

use std::fmt::Write as _;

/// Returns whether `character` is a control character, a Unicode format
/// character such as a bidirectional override, a zero-width space, or a
/// tag character, or another character that renders as nothing, such as a
/// variation selector, a Hangul filler, or an unassigned default-ignorable
/// code point. Any of them could hide text from
/// a person reading untrusted text while a model still reads it.
pub fn is_hidden_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

/// Returns `text` as a JSON string in which `<`, `>`, `&`, `@`, `[`, and
/// `]` appear only as `\u` escapes, so the encoded text can close no tag
/// and holds no path mention or image placeholder.
pub fn escaped_json_string(text: &str) -> String {
    let encoded = serde_json::Value::String(text.to_string()).to_string();
    let mut escaped = String::with_capacity(encoded.len());
    for character in encoded.chars() {
        match character {
            '<' | '>' | '&' | '@' | '[' | ']' => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }

    escaped
}

#[cfg(test)]
#[path = "text_test.rs"]
mod tests;
