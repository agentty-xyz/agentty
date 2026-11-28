//! Contrast ratio and minimum unit tests.

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
            "silver": {
                "$value": color("#767676", [0.4627, 0.4627, 0.4627]),
                "$extensions": { "xyz.agentty.highContrast": {
                    "green": color("#595959", [0.349, 0.349, 0.349]),
                    "dark": color("#cccccc", [0.8, 0.8, 0.8])
                } }
            },
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
fn pairs_that_reach_the_minimum_in_every_theme_pass() {
    // Arrange
    let contrast = json!([{
        "use": "Text",
        "minimum": 4.5,
        "foregrounds": ["{color.white}"],
        "backgrounds": ["{color.black}"]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(errors, Vec::<String>::new());
}

#[test]
fn every_pair_below_the_minimum_fails_in_each_theme() {
    // Arrange
    let contrast = json!([{
        "use": "Outlines",
        "minimum": 4.6,
        "foregrounds": ["{color.grey}"],
        "backgrounds": ["{color.white}", "{color.black}"]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "green: {color.grey} on {color.white} (Outlines) is 4.54:1, below 4.6:1; change a \
             colour",
            "dark: {color.grey} on {color.black} (Outlines) is 1.66:1, below 4.6:1; change a \
             colour",
        ]
    );
}

#[test]
fn a_high_contrast_group_checks_pairs_that_high_contrast_leaves_unchanged() {
    // Arrange
    let contrast = json!([{
        "use": "Text",
        "contrast": "high",
        "minimum": 4.6,
        "foregrounds": ["{color.grey}"],
        "backgrounds": ["{color.white}"]
    }]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "green, high contrast: {color.grey} on {color.white} (Text) is 4.54:1, below 4.6:1; \
             change a colour"
        ]
    );
}

#[test]
fn high_contrast_values_are_checked_against_every_minimum_that_applies() {
    // Arrange
    let contrast = json!([
        {
            "use": "Text",
            "minimum": 4.5,
            "foregrounds": ["{color.silver}"],
            "backgrounds": ["{color.white}"]
        },
        {
            "use": "Text at high contrast",
            "contrast": "high",
            "minimum": 7,
            "foregrounds": ["{color.silver}"],
            "backgrounds": ["{color.white}", "{color.black}"]
        },
        {
            "use": "Text at standard contrast",
            "contrast": "standard",
            "minimum": 4.6,
            "foregrounds": ["{color.silver}"],
            "backgrounds": ["{color.white}"]
        }
    ]);

    // Act
    let errors = errors(&contrast);

    // Assert
    assert_eq!(
        errors,
        [
            "dark, high contrast: {color.silver} on {color.white} (Text) is 1.61:1, below 4.5:1; \
             change a colour",
            "dark, high contrast: {color.silver} on {color.white} (Text at high contrast) is \
             1.61:1, below 7:1; change a colour",
            "green, high contrast: {color.silver} on {color.black} (Text at high contrast) is \
             3.00:1, below 7:1; change a colour",
            "green: {color.silver} on {color.white} (Text at standard contrast) is 4.54:1, below \
             4.6:1; change a colour",
            "dark: {color.silver} on {color.white} (Text at standard contrast) is 4.54:1, below \
             4.6:1; change a colour",
        ]
    );
}

#[test]
fn a_shortfall_record_is_rejected_as_an_unknown_field() {
    // Arrange
    let source = json!({
        "color": { "$type": "color", "white": { "$value": color("#ffffff", [1.0, 1.0, 1.0]) } },
        "$extensions": { "xyz.agentty.contrast": [{
            "use": "Text",
            "minimum": 4.5,
            "foregrounds": ["{color.white}"],
            "backgrounds": ["{color.white}"],
            "shortfalls": []
        }] }
    });

    // Act
    let parsed = TokenSet::parse(&source.to_string());

    // Assert
    let errors = parsed.err().expect("a shortfall record fails");
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].starts_with("xyz.agentty.contrast: unknown field `shortfalls`"),
        "{errors:?}"
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
