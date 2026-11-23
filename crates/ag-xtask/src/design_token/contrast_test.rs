//! Contrast ratio and shortfall record unit tests.

use serde_json::{Value as Json, json};

use super::{check, ratio};
use crate::design_token::token::TokenSet;

/// Returns an opaque sRGB colour value for `hex`.
fn color(hex: &str, components: [f64; 3]) -> Json {
    json!({ "colorSpace": "srgb", "components": components, "hex": hex })
}

/// Parses a fixture with white text, a grey that is dark only in the dark
/// theme, black, a translucent wash, and a radius, under `contrast`.
fn set(contrast: &Json) -> TokenSet {
    let source = json!({
        "color": {
            "$type": "color",
            "white": { "$value": color("#ffffff", [1.0, 1.0, 1.0]) },
            "grey": {
                "$value": color("#767676", [0.4627, 0.4627, 0.4627]),
                "$extensions": { "xyz.agentty.theme": { "dark": color("#333333", [0.2, 0.2, 0.2]) } }
            },
            "black": { "$value": color("#000000", [0.0, 0.0, 0.0]) },
            "wash": { "$value": { "colorSpace": "srgb", "components": [0, 0, 0], "alpha": 0.5, "hex": "#000000" } }
        },
        "radius": { "sm": { "$type": "dimension", "$value": { "value": 1, "unit": "px" } } },
        "$extensions": { "xyz.agentty.contrast": contrast }
    });

    TokenSet::parse(&source.to_string()).expect("valid tokens")
}

fn errors(contrast: &Json) -> Vec<String> {
    let mut errors = Vec::new();
    check(&set(contrast), &mut errors);

    errors
}

#[test]
fn ratio_matches_wcag_reference_values() {
    // Arrange
    let pairs = [
        ([0, 0, 0], [255, 255, 255]),
        ([255, 255, 255], [0, 0, 0]),
        ([118, 118, 118], [255, 255, 255]),
        ([7, 11, 8], [7, 11, 8]),
    ];

    // Act
    let ratios: Vec<String> = pairs
        .into_iter()
        .map(|(first, second)| format!("{:.2}", ratio(first, second)))
        .collect();

    // Assert
    assert_eq!(ratios, ["21.00", "21.00", "4.54", "1.00"]);
}

#[test]
fn pairs_that_reach_the_minimum_or_record_their_shortfall_pass() {
    // Arrange
    let contrast = json!([{
        "use": "Text",
        "minimum": 4.5,
        "foregrounds": ["{color.grey}"],
        "backgrounds": ["{color.white}", "{color.black}"],
        "shortfalls": [
            { "theme": "dark", "foreground": "{color.grey}", "background": "{color.black}", "ratio": 1.66, "reason": "Fixture." },
            { "theme": "dark", "foreground": "{color.grey}", "background": "{color.white}", "ratio": 12.63, "reason": "Fixture." },
            { "theme": "green", "foreground": "{color.grey}", "background": "{color.black}", "ratio": 4.62, "reason": "Fixture." }
        ]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "dark: {color.grey} on {color.white} (Text) now reaches 12.63:1; remove its shortfall",
            "green: {color.grey} on {color.black} (Text) now reaches 4.62:1; remove its shortfall",
        ]
    );
}

#[test]
fn pairs_below_the_minimum_need_an_exact_shortfall_record() {
    // Arrange
    let contrast = json!([{
        "use": "Outlines",
        "minimum": 7,
        "foregrounds": ["{color.grey}"],
        "backgrounds": ["{color.white}", "{color.black}"],
        "shortfalls": [
            { "theme": "green", "foreground": "{color.grey}", "background": "{color.black}", "ratio": 4.6, "reason": "Fixture." },
            { "theme": "dark", "foreground": "{color.grey}", "background": "{color.black}", "ratio": 1.66, "reason": " " },
            { "theme": "dark", "foreground": "{color.white}", "background": "{color.black}", "ratio": 21, "reason": "Fixture." }
        ]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "green: {color.grey} on {color.white} (Outlines) is 4.54:1, below 7:1; change a \
             colour or record a shortfall with its reason",
            "green: {color.grey} on {color.black} (Outlines) is 4.62:1, but its shortfall records \
             4.60; update the record",
            "dark: shortfall {color.grey} on {color.black} (Outlines) needs a reason",
            "dark: shortfall {color.white} on {color.black} (Outlines) matches no pair of the \
             group",
        ]
    );
}

#[test]
fn references_must_name_opaque_colour_tokens() {
    // Arrange
    let contrast = json!([{
        "use": "Text",
        "minimum": 3,
        "foregrounds": ["color.white", "{color.missing}", "{color.wash}", "{radius.sm}", "{color.white}"],
        "backgrounds": ["{color.black}"]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "contrast (Text): color.white must reference an opaque colour token, such as \
             {color.bg}",
            "contrast (Text): {color.missing} must reference an opaque colour token, such as \
             {color.bg}",
            "contrast (Text): {color.wash} must reference an opaque colour token, such as \
             {color.bg}",
            "contrast (Text): {radius.sm} must reference an opaque colour token, such as \
             {color.bg}",
        ]
    );
}
