//! W3C Design Tokens Format Module parsing for the token types the web
//! surfaces use, and rendering of a surface's tokens as CSS custom
//! properties.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value as Json};

/// Theme whose values `$value` holds.
pub(super) const DEFAULT_THEME: &str = "green";

/// The other theme, selected by the `theme-dark` class on the root element.
pub(super) const DARK_THEME: &str = "dark";

/// Every theme, default first.
pub(super) const THEMES: [&str; 2] = [DEFAULT_THEME, DARK_THEME];

/// Token extension that holds the value in themes other than the default.
const THEME_EXTENSION: &str = "xyz.agentty.theme";

/// Token extension that holds the value in each theme when the system asks
/// for more contrast.
const HIGH_CONTRAST_EXTENSION: &str = "xyz.agentty.highContrast";

/// Media query that selects the high-contrast values on a web page.
const HIGH_CONTRAST_MEDIA: &str = "@media (prefers-contrast: more)";

/// Root extension that lists the colour pairs whose contrast is checked.
const CONTRAST_EXTENSION: &str = "xyz.agentty.contrast";

/// Root extension that lists the self-hosted font faces.
const FONT_FACE_EXTENSION: &str = "xyz.agentty.fontFaces";

/// Token extension that names the CSS unit a `number` token is rendered in,
/// for values such as letter spacing in `em` that DTCG dimensions cannot hold.
const UNIT_EXTENSION: &str = "xyz.agentty.unit";

/// CSS units a `number` token may name.
const NUMBER_UNITS: [&str; 1] = ["em"];

/// DTCG font weight aliases and the weight each names.
const FONT_WEIGHT_ALIASES: [(u64, &[&str]); 10] = [
    (100, &["thin", "hairline"]),
    (200, &["extra-light", "ultra-light"]),
    (300, &["light"]),
    (400, &["normal", "regular", "book"]),
    (500, &["medium"]),
    (600, &["semi-bold", "demi-bold"]),
    (700, &["bold"]),
    (800, &["extra-bold", "ultra-bold"]),
    (900, &["black", "heavy"]),
    (950, &["extra-black", "ultra-black"]),
];

/// Root group whose child groups hold the values only one surface uses.
const SURFACES: &str = "surfaces";

/// Cubic Bézier curves that CSS names with a keyword.
const TIMING_KEYWORDS: [([f64; 4], &str); 5] = [
    ([0.25, 0.1, 0.25, 1.0], "ease"),
    ([0.0, 0.0, 1.0, 1.0], "linear"),
    ([0.42, 0.0, 1.0, 1.0], "ease-in"),
    ([0.0, 0.0, 0.58, 1.0], "ease-out"),
    ([0.42, 0.0, 0.58, 1.0], "ease-in-out"),
];

/// Parsed token file.
pub(super) struct TokenSet {
    pub(super) contrast: Vec<ContrastGroup>,
    pub(super) font_faces: Vec<FontFace>,
    pub(super) tokens: Vec<Token>,
}

impl TokenSet {
    /// Parses a token file, reporting every invalid token, duplicate custom
    /// property name, and invalid root extension.
    pub(super) fn parse(source: &str) -> Result<Self, Vec<String>> {
        let root = match serde_json::from_str::<Json>(source) {
            Ok(Json::Object(root)) => root,
            Ok(_) => return Err(vec!["expected a group at the root".to_owned()]),
            Err(error) => return Err(vec![format!("invalid JSON: {error}")]),
        };
        let mut parser = Parser::default();
        parser.group(&root, &Scope::default());
        let contrast = Self::extension(&root, CONTRAST_EXTENSION, &mut parser.errors);
        let font_faces = Self::extension(&root, FONT_FACE_EXTENSION, &mut parser.errors);
        parser.check_names();
        if !parser.errors.is_empty() {
            return Err(parser.errors);
        }

        Ok(Self {
            contrast,
            font_faces,
            tokens: parser.tokens,
        })
    }

    /// Reads the list a root extension holds, empty when the file has none.
    fn extension<T: DeserializeOwned>(
        root: &Map<String, Json>,
        name: &str,
        errors: &mut Vec<String>,
    ) -> Vec<T> {
        root.get("$extensions")
            .and_then(|extensions| extensions.get(name))
            .map_or(Ok(Vec::new()), Vec::<T>::deserialize)
            .unwrap_or_else(|error| {
                errors.push(format!("{name}: {error}"));

                Vec::new()
            })
    }

    /// Returns the token at a dotted `path`, such as `color.bg`.
    pub(super) fn find(&self, path: &str) -> Option<&Token> {
        self.tokens.iter().find(|token| token.path == path)
    }

    /// Returns the token behind custom property `--{name}` on `surface`: the
    /// surface's own when it replaces the shared one, as in its stylesheet,
    /// or else the shared one.
    pub(super) fn for_surface(&self, name: &str, surface: Option<&str>) -> Option<&Token> {
        let named = |token: &&Token| token.name == name;

        self.tokens
            .iter()
            .filter(named)
            .find(|token| token.surface.is_some() && token.surface.as_deref() == surface)
            .or_else(|| {
                self.tokens
                    .iter()
                    .filter(named)
                    .find(|token| token.surface.is_none())
            })
    }

    /// Returns the names of the surfaces that hold their own values.
    pub(super) fn surfaces(&self) -> BTreeSet<&str> {
        self.tokens
            .iter()
            .filter_map(|token| token.surface.as_deref())
            .collect()
    }

    /// Renders the shared tokens, with `surface`'s own tokens replacing the
    /// shared ones they rename and following the rest, as a stylesheet that
    /// sets them on the root element in each theme.
    pub(super) fn stylesheet(&self, header: &str, surface: Option<&str>) -> String {
        let mut selected: Vec<&Token> = self
            .tokens
            .iter()
            .filter(|token| token.surface.is_none())
            .collect();
        for token in self
            .tokens
            .iter()
            .filter(|token| token.surface.is_some() && token.surface.as_deref() == surface)
        {
            match selected.iter().position(|shared| shared.name == token.name) {
                Some(index) => selected[index] = token,
                None => selected.push(token),
            }
        }
        let mut text = format!("{header}\n");
        text.push_str(&Self::rule(
            ":root",
            selected.iter().map(|token| (*token, &token.value)),
        ));
        let dark: Vec<_> = selected
            .iter()
            .filter_map(|token| token.dark.as_ref().map(|value| (*token, value)))
            .collect();
        if !dark.is_empty() {
            text.push('\n');
            text.push_str(&Self::rule(":root.theme-dark", dark.into_iter()));
        }
        let high_contrast: Vec<_> = selected
            .iter()
            .filter_map(|token| token.high_contrast.as_ref().map(|values| (*token, values)))
            .collect();
        if !high_contrast.is_empty() {
            let rules = [
                Self::rule(
                    ":root",
                    high_contrast
                        .iter()
                        .map(|(token, values)| (*token, &values.default)),
                ),
                Self::rule(
                    ":root.theme-dark",
                    high_contrast
                        .iter()
                        .map(|(token, values)| (*token, &values.dark)),
                ),
            ];
            text.push('\n');
            text.push_str(HIGH_CONTRAST_MEDIA);
            text.push_str(" {\n");
            for line in rules.join("\n").lines() {
                if !line.is_empty() {
                    text.push_str("  ");
                    text.push_str(line);
                }
                text.push('\n');
            }
            text.push_str("}\n");
        }

        text
    }

    fn rule<'token>(
        selector: &str,
        declarations: impl Iterator<Item = (&'token Token, &'token Value)>,
    ) -> String {
        let mut text = format!("{selector} {{\n");
        text.extend(
            declarations.map(|(token, value)| format!("  --{}: {};\n", token.name, value.css())),
        );
        text.push_str("}\n");

        text
    }
}

/// One token, flattened from its group path.
pub(super) struct Token {
    pub(super) dark: Option<Value>,
    pub(super) high_contrast: Option<HighContrast>,
    pub(super) name: String,
    pub(super) path: String,
    pub(super) surface: Option<String>,
    pub(super) value: Value,
}

impl Token {
    /// Returns the value in `theme` at `contrast`: the high-contrast value
    /// when the token has one and the system asks for more contrast, and
    /// otherwise the theme's value, falling back to the default.
    pub(super) fn value_in(&self, theme: &str, contrast: Contrast) -> &Value {
        match (&self.high_contrast, &self.dark) {
            (Some(values), _) if contrast == Contrast::High && theme == DARK_THEME => &values.dark,
            (Some(values), _) if contrast == Contrast::High => &values.default,
            (_, Some(dark)) if theme == DARK_THEME => dark,
            _ => &self.value,
        }
    }
}

/// A token's values in each theme when the system asks for more contrast.
pub(super) struct HighContrast {
    pub(super) dark: Value,
    pub(super) default: Value,
}

/// The contrast the system asks for: on the web, `prefers-contrast: more`
/// selects `High`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum Contrast {
    High,
    Standard,
}

/// A token value: a colour, kept for contrast checks, or rendered CSS text.
#[derive(Debug, PartialEq)]
pub(super) enum Value {
    Color(Color),
    Css(String),
}

impl Value {
    fn parse(kind: &str, json: &Json) -> Result<Self, String> {
        match kind {
            "color" => Color::parse(json).map(Self::Color),
            "dimension" => Self::dimension(json).map(Self::Css),
            "fontFamily" => Self::font_family(json).map(Self::Css),
            "fontWeight" => Self::font_weight(json).map(Self::Css),
            "number" => json
                .as_number()
                .map(ToString::to_string)
                .map(Self::Css)
                .ok_or_else(|| "expected a number".to_owned()),
            "shadow" => Self::shadow(json).map(Self::Css),
            "transition" => Self::transition(json).map(Self::Css),
            _ => Err(format!("unsupported $type {kind}")),
        }
    }

    /// Returns the value as CSS text.
    pub(super) fn css(&self) -> String {
        match self {
            Self::Color(color) => color.css(),
            Self::Css(text) => text.clone(),
        }
    }

    /// Appends `unit` to a rendered number.
    fn in_unit(self, unit: Option<&str>) -> Self {
        match (self, unit) {
            (Self::Css(text), Some(unit)) => Self::Css(format!("{text}{unit}")),
            (value, _) => value,
        }
    }

    /// Renders a length, writing zero without a unit.
    fn dimension(json: &Json) -> Result<String, String> {
        let (amount, text) = Self::measure(json, &["px", "rem"])?;

        Ok(if amount == 0.0 { "0".to_owned() } else { text })
    }

    /// Reads a `{ value, unit }` object and returns the amount and its CSS
    /// text, keeping the number exactly as the file writes it.
    fn measure(json: &Json, units: &[&str]) -> Result<(f64, String), String> {
        let amount = json.get("value").and_then(Json::as_number);
        let unit = json
            .get("unit")
            .and_then(Json::as_str)
            .filter(|unit| units.contains(unit));
        match (amount.and_then(serde_json::Number::as_f64), amount, unit) {
            (Some(value), Some(number), Some(unit)) => Ok((value, format!("{number}{unit}"))),
            _ => Err(format!(
                "expected a value with a unit of {}",
                units.join(" or ")
            )),
        }
    }

    fn font_family(json: &Json) -> Result<String, String> {
        let names: Option<Vec<&str>> = match json {
            Json::String(name) => Some(vec![name.as_str()]),
            Json::Array(names) => names.iter().map(Json::as_str).collect(),
            _ => None,
        };
        let names = names.ok_or("expected a font name or a list of font names")?;

        Ok(names
            .into_iter()
            .map(|name| {
                if name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    name.to_owned()
                } else {
                    format!("\"{name}\"")
                }
            })
            .collect::<Vec<_>>()
            .join(", "))
    }

    /// Renders a weight from 1 to 1000, or one of its DTCG aliases, as a
    /// number.
    fn font_weight(json: &Json) -> Result<String, String> {
        let weight = match json {
            Json::Number(number) => number.as_u64().filter(|weight| (1..=1000).contains(weight)),
            Json::String(alias) => FONT_WEIGHT_ALIASES
                .iter()
                .find(|(_, names)| names.contains(&alias.as_str()))
                .map(|(weight, _)| *weight),
            _ => None,
        };

        weight
            .map(|weight| weight.to_string())
            .ok_or_else(|| "expected a weight from 1 to 1000 or a weight alias".to_owned())
    }

    fn shadow(json: &Json) -> Result<String, String> {
        let layers = match json {
            Json::Array(layers) => layers.iter().collect(),
            layer => vec![layer],
        };
        let layers: Result<Vec<_>, _> = layers.into_iter().map(Self::shadow_layer).collect();

        Ok(layers?.join(", "))
    }

    /// Renders one shadow, leaving out a zero spread so the same token also
    /// works as a `text-shadow`.
    fn shadow_layer(json: &Json) -> Result<String, String> {
        let color = Color::parse(json.get("color").unwrap_or(&Json::Null))
            .map_err(|error| format!("color: {error}"))?;
        let mut parts = Vec::new();
        if json.get("inset").and_then(Json::as_bool) == Some(true) {
            parts.push("inset".to_owned());
        }
        for key in ["offsetX", "offsetY", "blur", "spread"] {
            let length = Self::dimension(json.get(key).unwrap_or(&Json::Null))
                .map_err(|error| format!("{key}: {error}"))?;
            if key != "spread" || length != "0" {
                parts.push(length);
            }
        }
        parts.push(color.css());

        Ok(parts.join(" "))
    }

    /// Renders a transition as its duration, timing function, and any
    /// delay, the order the `transition` shorthand expects after a property.
    fn transition(json: &Json) -> Result<String, String> {
        let field = |key: &str| json.get(key).unwrap_or(&Json::Null);
        let (_, duration) = Self::measure(field("duration"), &["ms", "s"])
            .map_err(|error| format!("duration: {error}"))?;
        let (delay_amount, delay) = Self::measure(field("delay"), &["ms", "s"])
            .map_err(|error| format!("delay: {error}"))?;
        let mut parts = vec![duration, Self::timing(field("timingFunction"))?];
        if delay_amount != 0.0 {
            parts.push(delay);
        }

        Ok(parts.join(" "))
    }

    fn timing(json: &Json) -> Result<String, String> {
        let points: Option<Vec<f64>> = json
            .as_array()
            .filter(|points| points.len() == 4)
            .and_then(|points| points.iter().map(Json::as_f64).collect());
        let points = points.ok_or("timingFunction: expected four numbers")?;
        let keyword = TIMING_KEYWORDS.iter().find(|(curve, _)| {
            curve
                .iter()
                .zip(&points)
                .all(|(expected, actual)| (expected - actual).abs() < f64::EPSILON)
        });

        Ok(keyword.map_or_else(
            || {
                let points: Vec<String> = points.iter().map(ToString::to_string).collect();

                format!("cubic-bezier({})", points.join(", "))
            },
            |(_, keyword)| (*keyword).to_owned(),
        ))
    }
}

/// An sRGB colour with an optional alpha below one.
#[derive(Debug, PartialEq)]
pub(super) struct Color {
    alpha: Option<String>,
    rgb: [u8; 3],
}

impl Color {
    /// Parses a DTCG colour, which must carry a hex fallback that its
    /// components match.
    fn parse(json: &Json) -> Result<Self, String> {
        if json.get("colorSpace").and_then(Json::as_str) != Some("srgb") {
            return Err("expected an srgb colour".to_owned());
        }
        let rgb = json
            .get("hex")
            .and_then(Json::as_str)
            .and_then(Self::hex)
            .ok_or("needs a six-digit hex fallback")?;
        let components: Option<Vec<f64>> = json
            .get("components")
            .and_then(Json::as_array)
            .and_then(|components| components.iter().map(Json::as_f64).collect());
        let matches = components.is_some_and(|components| {
            components.len() == 3
                && components.iter().zip(rgb).all(|(component, channel)| {
                    (component * 255.0 - f64::from(channel)).abs() < 0.5
                })
        });
        if !matches {
            return Err("components must be the hex channels divided by 255".to_owned());
        }
        let alpha = match json.get("alpha") {
            None => None,
            Some(Json::Number(alpha))
                if alpha
                    .as_f64()
                    .is_some_and(|alpha| (0.0..=1.0).contains(&alpha)) =>
            {
                alpha
                    .as_f64()
                    .is_some_and(|alpha| alpha < 1.0)
                    .then(|| alpha.to_string())
            }
            Some(_) => return Err("alpha must be a number from 0 to 1".to_owned()),
        };

        Ok(Self { alpha, rgb })
    }

    fn hex(text: &str) -> Option<[u8; 3]> {
        let digits = text.strip_prefix('#').filter(|digits| {
            digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
        })?;
        let channel = |start: usize| u8::from_str_radix(&digits[start..start + 2], 16).ok();

        Some([channel(0)?, channel(2)?, channel(4)?])
    }

    /// Returns the colour as CSS: hex when opaque, `rgba()` otherwise.
    fn css(&self) -> String {
        let [red, green, blue] = self.rgb;
        match &self.alpha {
            None => format!("#{red:02x}{green:02x}{blue:02x}"),
            Some(alpha) => format!("rgba({red}, {green}, {blue}, {alpha})"),
        }
    }

    /// Returns the channels of an opaque colour.
    pub(super) fn opaque(&self) -> Option<[u8; 3]> {
        self.alpha.is_none().then_some(self.rgb)
    }
}

/// Colour pairs that must reach a contrast ratio in every theme.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContrastGroup {
    pub(super) backgrounds: Vec<String>,
    /// The one contrast level the pairs are checked at; without it, they
    /// are checked at every level.
    #[serde(default)]
    pub(super) contrast: Option<Contrast>,
    pub(super) foregrounds: Vec<String>,
    pub(super) minimum: f64,
    #[serde(rename = "use")]
    pub(super) usage: String,
}

/// A self-hosted font face: a WOFF2 file and its licence text, both relative
/// to the token file, and the copyright and licence notice that the
/// stylesheet repeats.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct FontFace {
    pub(super) family: String,
    pub(super) license: String,
    pub(super) notice: String,
    pub(super) source: String,
    pub(super) style: FontStyle,
    pub(super) weight: u16,
}

/// The `font-style` of a font face.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(super) enum FontStyle {
    Italic,
    Normal,
}

impl FontStyle {
    /// Returns the CSS keyword.
    pub(super) fn css(self) -> &'static str {
        match self {
            Self::Italic => "italic",
            Self::Normal => "normal",
        }
    }
}

/// Where the walk is in the token tree.
#[derive(Clone, Default)]
struct Scope {
    kind: Option<String>,
    name: Vec<String>,
    path: Vec<String>,
    surface: Option<String>,
}

impl Scope {
    fn child(&self, key: &str, kind: Option<String>) -> Self {
        let mut child = self.clone();
        child.kind = kind;
        child.name.push(key.to_owned());
        child.path.push(key.to_owned());

        child
    }
}

/// Tree walk that collects tokens and errors.
#[derive(Default)]
struct Parser {
    errors: Vec<String>,
    tokens: Vec<Token>,
}

impl Parser {
    fn group(&mut self, group: &Map<String, Json>, scope: &Scope) {
        let kind = group
            .get("$type")
            .and_then(Json::as_str)
            .map(str::to_owned)
            .or_else(|| scope.kind.clone());
        for (key, child) in group.iter().filter(|(key, _)| !key.starts_with('$')) {
            let scope = scope.child(key, kind.clone());
            let Json::Object(child) = child else {
                self.error(&scope, "expected a token or a group");
                continue;
            };
            if scope.path.len() == 1 && key == SURFACES {
                self.surfaces(child, &scope);
            } else if child.contains_key("$value") {
                self.token(child, &scope);
            } else {
                self.group(child, &scope);
            }
        }
    }

    /// Walks each surface's group, naming its tokens as if they were shared.
    fn surfaces(&mut self, surfaces: &Map<String, Json>, scope: &Scope) {
        for (surface, group) in surfaces.iter().filter(|(key, _)| !key.starts_with('$')) {
            let mut surface_scope = scope.child(surface, None);
            surface_scope.name.clear();
            surface_scope.surface = Some(surface.clone());
            match group {
                Json::Object(group) => self.group(group, &surface_scope),
                _ => self.error(&surface_scope, "expected a surface group"),
            }
        }
    }

    fn token(&mut self, token: &Map<String, Json>, scope: &Scope) {
        let kind = token
            .get("$type")
            .and_then(Json::as_str)
            .or(scope.kind.as_deref());
        let Some(kind) = kind else {
            self.error(scope, "has no $type");
            return;
        };
        let value = Value::parse(kind, &token["$value"]);
        let dark = Self::dark(kind, token);
        let high_contrast = Self::high_contrast(kind, token);
        let unit = Self::unit(kind, token);
        match (value, dark, high_contrast, unit) {
            (Ok(value), Ok(dark), Ok(high_contrast), Ok(unit)) => self.tokens.push(Token {
                dark: dark.map(|dark| dark.in_unit(unit)),
                high_contrast: high_contrast.map(|values| HighContrast {
                    dark: values.dark.in_unit(unit),
                    default: values.default.in_unit(unit),
                }),
                name: scope.name.join("-"),
                path: scope.path.join("."),
                surface: scope.surface.clone(),
                value: value.in_unit(unit),
            }),
            (value, dark, high_contrast, unit) => {
                let errors = [value.err(), dark.err()]
                    .into_iter()
                    .flatten()
                    .chain(high_contrast.err().into_iter().flatten())
                    .chain(unit.err());
                for error in errors {
                    self.error(scope, &error);
                }
            }
        }
    }

    /// Reads the value in each theme at high contrast from the high-contrast
    /// extension, which names every theme.
    fn high_contrast(
        kind: &str,
        token: &Map<String, Json>,
    ) -> Result<Option<HighContrast>, Vec<String>> {
        let Some(themes) = token
            .get("$extensions")
            .and_then(|extensions| extensions.get(HIGH_CONTRAST_EXTENSION))
        else {
            return Ok(None);
        };
        let themes = themes.as_object().ok_or_else(|| {
            vec!["the high-contrast extension must map theme names to values".to_owned()]
        })?;
        if let Some(theme) = themes
            .keys()
            .find(|theme| !THEMES.contains(&theme.as_str()))
        {
            return Err(vec![format!(
                "the high-contrast extension names unknown theme {theme}; it names {}",
                THEMES.join(" and ")
            )]);
        }
        let value = |theme: &str| {
            themes
                .get(theme)
                .ok_or_else(|| format!("the high-contrast extension has no {theme} value"))
                .and_then(|value| {
                    Value::parse(kind, value)
                        .map_err(|error| format!("high contrast {theme}: {error}"))
                })
        };
        match (value(DEFAULT_THEME), value(DARK_THEME)) {
            (Ok(default), Ok(dark)) => Ok(Some(HighContrast { dark, default })),
            (default, dark) => Err([default.err(), dark.err()].into_iter().flatten().collect()),
        }
    }

    /// Reads the CSS unit a `number` token names in the unit extension.
    fn unit<'token>(
        kind: &str,
        token: &'token Map<String, Json>,
    ) -> Result<Option<&'token str>, String> {
        let Some(unit) = token
            .get("$extensions")
            .and_then(|extensions| extensions.get(UNIT_EXTENSION))
        else {
            return Ok(None);
        };

        unit.as_str()
            .filter(|unit| kind == "number" && NUMBER_UNITS.contains(unit))
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "the unit extension gives a number token one of the units {}",
                    NUMBER_UNITS.join(", ")
                )
            })
    }

    /// Reads the dark theme's value from the theme extension.
    fn dark(kind: &str, token: &Map<String, Json>) -> Result<Option<Value>, String> {
        let Some(themes) = token
            .get("$extensions")
            .and_then(|extensions| extensions.get(THEME_EXTENSION))
        else {
            return Ok(None);
        };
        let themes = themes
            .as_object()
            .ok_or("the theme extension must map theme names to values")?;
        if let Some(theme) = themes.keys().find(|theme| *theme != DARK_THEME) {
            return Err(format!(
                "names unknown theme {theme}; the default theme is $value and the other is \
                 {DARK_THEME}"
            ));
        }
        themes
            .get(DARK_THEME)
            .map(|value| {
                Value::parse(kind, value).map_err(|error| format!("{DARK_THEME}: {error}"))
            })
            .transpose()
    }

    /// Reports custom property names that two tokens of the same scope share.
    fn check_names(&mut self) {
        let mut seen = BTreeSet::new();
        for token in &self.tokens {
            if !seen.insert((token.surface.as_deref(), token.name.as_str())) {
                self.errors.push(format!(
                    "{}: --{} is already defined",
                    token.path, token.name
                ));
            }
        }
    }

    fn error(&mut self, scope: &Scope, message: &str) {
        self.errors
            .push(format!("{}: {message}", scope.path.join(".")));
    }
}

#[cfg(test)]
#[path = "token_test.rs"]
mod tests;
