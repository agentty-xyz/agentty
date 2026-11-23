//! Design token parsing and stylesheet rendering unit tests.

use serde_json::{Value as Json, json};

use super::{DARK_THEME, DEFAULT_THEME, FontFace, FontStyle, TokenSet, Value};

/// Returns an opaque sRGB colour value for `hex`.
fn color(hex: &str, components: [f64; 3]) -> Json {
    json!({ "colorSpace": "srgb", "components": components, "hex": hex })
}

fn errors(source: &str) -> Vec<String> {
    TokenSet::parse(source).err().expect("token errors")
}

#[test]
fn stylesheet_sets_shared_tokens_with_surface_values_in_each_theme() {
    // Arrange
    let source = json!({
        "$description": "Fixture tokens.",
        "color": {
            "$type": "color",
            "bg": {
                "$value": color("#070B08", [0.0275, 0.0431, 0.0314]),
                "$extensions": { "xyz.agentty.theme": { "dark": color("#080808", [0.0314, 0.0314, 0.0314]) } }
            },
            "line": { "$value": color("#000000", [0.0, 0.0, 0.0]), "$extensions": { "other": true } }
        },
        "font": { "mono": { "$type": "fontFamily", "$value": "monospace" } },
        "surfaces": {
            "$description": "Values one surface uses.",
            "web": {
                "font": { "$type": "fontFamily", "mono": { "$value": ["Menlo", "monospace"] } },
                "glyph": {
                    "$type": "color",
                    "0": {
                        "$value": color("#ffffff", [1.0, 1.0, 1.0]),
                        "$extensions": { "xyz.agentty.theme": { "dark": color("#000000", [0.0, 0.0, 0.0]) } }
                    }
                }
            },
            "print": { "radius": { "sm": { "$type": "dimension", "$value": { "value": 1, "unit": "px" } } } }
        }
    })
    .to_string();

    // Act
    let set = TokenSet::parse(&source).expect("valid tokens");
    let shared = set.stylesheet("/* Shared. */", None);
    let web = set.stylesheet("/* Web. */", Some("web"));

    // Assert
    assert_eq!(
        shared,
        "/* Shared. */\n:root {\n  --color-bg: #070b08;\n  --color-line: #000000;\n  --font-mono: \
         monospace;\n}\n\n:root.theme-dark {\n  --color-bg: #080808;\n}\n"
    );
    assert_eq!(
        web,
        "/* Web. */\n:root {\n  --color-bg: #070b08;\n  --color-line: #000000;\n  --font-mono: \
         Menlo, monospace;\n  --glyph-0: #ffffff;\n}\n\n:root.theme-dark {\n  --color-bg: \
         #080808;\n  --glyph-0: #000000;\n}\n"
    );
    assert_eq!(
        set.surfaces().into_iter().collect::<Vec<_>>(),
        ["print", "web"]
    );
    assert_eq!(
        set.find("surfaces.web.glyph.0")
            .map(|token| token.name.as_str()),
        Some("glyph-0")
    );
    assert!(set.find("glyph.0").is_none());
    assert!(set.contrast.is_empty());
    assert_eq!(set.font_faces, Vec::<FontFace>::new());
}

#[test]
fn font_faces_are_read_from_the_root_extension() {
    // Arrange
    let source = json!({
        "$extensions": { "xyz.agentty.fontFaces": [
            { "family": "Mono", "style": "italic", "weight": 700, "source": "fonts/Mono.woff2", "license": "fonts/OFL.txt", "notice": "Copyright." }
        ] }
    })
    .to_string();
    let unknown = json!({
        "$extensions": { "xyz.agentty.fontFaces": [
            { "family": "Mono", "style": "oblique", "weight": 700, "source": "a.woff2", "license": "a.txt", "notice": "Copyright." }
        ] }
    })
    .to_string();

    // Act
    let set = TokenSet::parse(&source).expect("valid font faces");
    let report = errors(&unknown);

    // Assert
    assert_eq!(
        set.font_faces,
        [FontFace {
            family: "Mono".to_owned(),
            license: "fonts/OFL.txt".to_owned(),
            notice: "Copyright.".to_owned(),
            source: "fonts/Mono.woff2".to_owned(),
            style: FontStyle::Italic,
            weight: 700,
        }]
    );
    assert_eq!(report.len(), 1);
    assert!(
        report[0].starts_with("xyz.agentty.fontFaces: unknown variant `oblique`"),
        "{report:?}"
    );
}

#[test]
fn stylesheet_without_dark_values_sets_only_the_default_theme() {
    // Arrange
    let source = json!({ "radius": { "$type": "dimension", "sm": { "$value": { "value": 0.35, "unit": "rem" } } } })
        .to_string();

    // Act
    let set = TokenSet::parse(&source).expect("valid tokens");

    // Assert
    assert_eq!(
        set.stylesheet("/* Header. */", None),
        "/* Header. */\n:root {\n  --radius-sm: 0.35rem;\n}\n"
    );
}

#[test]
fn number_tokens_render_in_the_unit_their_extension_names() {
    // Arrange
    let unit = |unit: Json| json!({ "xyz.agentty.unit": unit });
    let source = json!({
        "tracking": {
            "$type": "number",
            "wide": { "$value": 0.04, "$extensions": unit(json!("em")) },
            "themed": {
                "$value": 0.01,
                "$extensions": { "xyz.agentty.unit": "em", "xyz.agentty.theme": { "dark": 0.02 } }
            }
        },
        "line-height": { "body": { "$type": "number", "$value": 1.6 } }
    })
    .to_string();
    let invalid = json!({
        "tracking": {
            "$type": "number",
            "px": { "$value": 1, "$extensions": unit(json!("px")) },
            "list": { "$value": 1, "$extensions": unit(json!(["em"])) }
        },
        "radius": { "sm": { "$type": "dimension", "$value": { "value": 1, "unit": "px" }, "$extensions": unit(json!("em")) } }
    })
    .to_string();

    // Act
    let set = TokenSet::parse(&source).expect("valid tokens");
    let report = errors(&invalid);

    // Assert
    assert_eq!(
        set.stylesheet("/* Header. */", None),
        "/* Header. */\n:root {\n  --tracking-wide: 0.04em;\n  --tracking-themed: 0.01em;\n  \
         --line-height-body: 1.6;\n}\n\n:root.theme-dark {\n  --tracking-themed: 0.02em;\n}\n"
    );
    assert_eq!(
        report,
        [
            "tracking.px: the unit extension gives a number token one of the units em",
            "tracking.list: the unit extension gives a number token one of the units em",
            "radius.sm: the unit extension gives a number token one of the units em",
        ]
    );
}

#[test]
fn value_in_returns_the_dark_value_only_in_the_dark_theme() {
    // Arrange
    let source = json!({
        "color": {
            "$type": "color",
            "bg": {
                "$value": color("#ffffff", [1.0, 1.0, 1.0]),
                "$extensions": { "xyz.agentty.theme": { "dark": color("#000000", [0.0, 0.0, 0.0]) } }
            },
            "text": { "$value": color("#ffffff", [1.0, 1.0, 1.0]) }
        }
    })
    .to_string();
    let set = TokenSet::parse(&source).expect("valid tokens");
    let (bg, text) = (
        set.find("color.bg").expect("bg"),
        set.find("color.text").expect("text"),
    );

    // Act
    let values = [
        bg.value_in(DEFAULT_THEME).css(),
        bg.value_in(DARK_THEME).css(),
        text.value_in(DARK_THEME).css(),
    ];

    // Assert
    assert_eq!(values, ["#ffffff", "#000000", "#ffffff"]);
}

#[test]
fn values_render_as_css_text() {
    // Arrange
    let cases = [
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0], "alpha": 0.72, "hex": "#000000" }),
            "rgba(0, 0, 0, 0.72)",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [1, 1, 1], "alpha": 1, "hex": "#FFFFFF" }),
            "#ffffff",
        ),
        ("dimension", json!({ "value": 0, "unit": "rem" }), "0"),
        (
            "dimension",
            json!({ "value": 0.35, "unit": "rem" }),
            "0.35rem",
        ),
        ("fontFamily", json!("Inter"), "Inter"),
        ("fontWeight", json!(650), "650"),
        ("fontWeight", json!("semi-bold"), "600"),
        ("number", json!(1.25), "1.25"),
        ("number", json!(-0.025), "-0.025"),
        (
            "fontFamily",
            json!(["Segoe UI", "SFMono-Regular", "-apple-system"]),
            "\"Segoe UI\", SFMono-Regular, -apple-system",
        ),
        (
            "shadow",
            json!({
                "color": { "colorSpace": "srgb", "components": [0, 0, 0], "alpha": 0.34, "hex": "#000000" },
                "offsetX": { "value": 0, "unit": "px" },
                "offsetY": { "value": 24, "unit": "px" },
                "blur": { "value": 70, "unit": "px" },
                "spread": { "value": 0, "unit": "px" }
            }),
            "0 24px 70px rgba(0, 0, 0, 0.34)",
        ),
        (
            "shadow",
            json!([
                {
                    "color": color("#ffffff", [1.0, 1.0, 1.0]),
                    "offsetX": { "value": 1, "unit": "px" },
                    "offsetY": { "value": 2, "unit": "px" },
                    "blur": { "value": 3, "unit": "px" },
                    "spread": { "value": 4, "unit": "px" },
                    "inset": true
                },
                {
                    "color": color("#000000", [0.0, 0.0, 0.0]),
                    "offsetX": { "value": 0, "unit": "px" },
                    "offsetY": { "value": 0, "unit": "px" },
                    "blur": { "value": 1, "unit": "rem" },
                    "spread": { "value": 0, "unit": "px" },
                    "inset": false
                }
            ]),
            "inset 1px 2px 3px 4px #ffffff, 0 0 1rem #000000",
        ),
        (
            "transition",
            json!({ "duration": { "value": 140, "unit": "ms" }, "delay": { "value": 0, "unit": "ms" }, "timingFunction": [0.25, 0.1, 0.25, 1] }),
            "140ms ease",
        ),
        (
            "transition",
            json!({ "duration": { "value": 0.2, "unit": "s" }, "delay": { "value": 50, "unit": "ms" }, "timingFunction": [0.5, 0, 1, 1] }),
            "0.2s cubic-bezier(0.5, 0, 1, 1) 50ms",
        ),
    ];

    // Act
    let rendered: Vec<_> = cases
        .iter()
        .map(|(kind, json, _)| Value::parse(kind, json).map(|value| value.css()))
        .collect();

    // Assert
    for ((kind, _, expected), rendered) in cases.iter().zip(rendered) {
        assert_eq!(rendered.as_deref(), Ok(*expected), "{kind}");
    }
}

/// Returns the parse error of each `(kind, value, expected error)` case.
fn parse_errors(cases: &[(&str, Json, &str)]) -> Vec<Option<String>> {
    cases
        .iter()
        .map(|(kind, json, _)| Value::parse(kind, json).err())
        .collect()
}

#[test]
fn invalid_colours_name_what_is_expected() {
    // Arrange
    let cases = [
        ("typography", json!(1), "unsupported $type typography"),
        (
            "color",
            json!({ "colorSpace": "hsl", "components": [0, 0, 0], "hex": "#000000" }),
            "expected an srgb colour",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0] }),
            "needs a six-digit hex fallback",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0], "hex": "#00000g" }),
            "needs a six-digit hex fallback",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0.5], "hex": "#000000" }),
            "components must be the hex channels divided by 255",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0], "hex": "#000000" }),
            "components must be the hex channels divided by 255",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0], "alpha": 2, "hex": "#000000" }),
            "alpha must be a number from 0 to 1",
        ),
        (
            "color",
            json!({ "colorSpace": "srgb", "components": [0, 0, 0], "alpha": "half", "hex": "#000000" }),
            "alpha must be a number from 0 to 1",
        ),
    ];

    // Act
    let errors = parse_errors(&cases);

    // Assert
    for ((kind, _, expected), error) in cases.iter().zip(errors) {
        assert_eq!(error.as_deref(), Some(*expected), "{kind}");
    }
}

#[test]
fn invalid_values_name_what_is_expected() {
    // Arrange
    let shadow = |field: &str, value: Json| {
        let mut shadow = json!({
            "color": color("#000000", [0.0, 0.0, 0.0]),
            "offsetX": { "value": 0, "unit": "px" },
            "offsetY": { "value": 0, "unit": "px" },
            "blur": { "value": 0, "unit": "px" },
            "spread": { "value": 0, "unit": "px" }
        });
        shadow[field] = value;

        shadow
    };
    let transition = |field: &str, value: Json| {
        let mut transition = json!({
            "duration": { "value": 1, "unit": "ms" },
            "delay": { "value": 0, "unit": "ms" },
            "timingFunction": [0, 0, 1, 1]
        });
        transition[field] = value;

        transition
    };
    let cases = [
        (
            "dimension",
            json!({ "value": 1, "unit": "em" }),
            "expected a value with a unit of px or rem",
        ),
        (
            "fontFamily",
            json!(1),
            "expected a font name or a list of font names",
        ),
        (
            "fontFamily",
            json!(["Inter", 1]),
            "expected a font name or a list of font names",
        ),
        (
            "fontWeight",
            json!(1001),
            "expected a weight from 1 to 1000 or a weight alias",
        ),
        (
            "fontWeight",
            json!("Bold"),
            "expected a weight from 1 to 1000 or a weight alias",
        ),
        (
            "fontWeight",
            json!([400]),
            "expected a weight from 1 to 1000 or a weight alias",
        ),
        ("number", json!("1.5"), "expected a number"),
        (
            "shadow",
            shadow("color", json!("#000000")),
            "color: expected an srgb colour",
        ),
        (
            "shadow",
            shadow("blur", json!({ "value": 1 })),
            "blur: expected a value with a unit of px or rem",
        ),
        (
            "transition",
            transition("duration", json!({ "value": 1, "unit": "px" })),
            "duration: expected a value with a unit of ms or s",
        ),
        (
            "transition",
            transition("delay", json!(0)),
            "delay: expected a value with a unit of ms or s",
        ),
        (
            "transition",
            transition("timingFunction", json!([0, 0, 1])),
            "timingFunction: expected four numbers",
        ),
    ];

    // Act
    let errors = parse_errors(&cases);

    // Assert
    for ((kind, _, expected), error) in cases.iter().zip(errors) {
        assert_eq!(error.as_deref(), Some(*expected), "{kind}");
    }
}

#[test]
fn unreadable_files_report_their_shape() {
    // Arrange
    let sources = ["{", "[]"];

    // Act
    let reports: Vec<_> = sources.into_iter().map(errors).collect();

    // Assert
    assert!(
        reports[0][0].starts_with("invalid JSON: "),
        "{:?}",
        reports[0]
    );
    assert_eq!(reports[1], ["expected a group at the root"]);
}

#[test]
fn invalid_tokens_report_every_problem_with_its_path() {
    // Arrange
    let black = color("#000000", [0.0, 0.0, 0.0]);
    let source = json!({
        "loose": 1,
        "untyped": { "$value": "1px" },
        "color": {
            "$type": "color",
            "both": { "$value": "red", "$extensions": { "xyz.agentty.theme": { "dark": "blue" } } },
            "shape": { "$value": black, "$extensions": { "xyz.agentty.theme": [] } },
            "theme": { "$value": black, "$extensions": { "xyz.agentty.theme": { "light": black } } },
            "twin": { "$value": black },
            "twin-name": { "$value": black }
        },
        "color-twin": { "name": { "$type": "color", "$value": black } },
        "surfaces": { "web": 1 },
        "$extensions": { "xyz.agentty.contrast": [{ "use": "Text" }] }
    })
    .to_string();

    // Act
    let report = errors(&source);

    // Assert
    assert_eq!(
        report,
        [
            "loose: expected a token or a group",
            "untyped: has no $type",
            "color.both: expected an srgb colour",
            "color.both: dark: expected an srgb colour",
            "color.shape: the theme extension must map theme names to values",
            "color.theme: names unknown theme light; the default theme is $value and the other is \
             dark",
            "surfaces.web: expected a surface group",
            "xyz.agentty.contrast: missing field `backgrounds`",
            "color-twin.name: --color-twin-name is already defined",
        ]
    );
}
