//! W3C Design Tokens Format Module parsing for the token types the web
//! surfaces use, and rendering of a surface's tokens as CSS custom
//! properties.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{Map, Value as Json};

/// Theme whose values `$value` holds.
pub(super) const DEFAULT_THEME: &str = "green";

/// The other theme, selected by the `theme-dark` class on the root element.
pub(super) const DARK_THEME: &str = "dark";

/// Every theme, default first.
pub(super) const THEMES: [&str; 2] = [DEFAULT_THEME, DARK_THEME];

/// Token extension that holds the value in themes other than the default.
const THEME_EXTENSION: &str = "xyz.agentty.theme";

/// Root extension that lists the colour pairs whose contrast is checked.
const CONTRAST_EXTENSION: &str = "xyz.agentty.contrast";

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
    pub(super) tokens: Vec<Token>,
}

impl TokenSet {
    /// Parses a token file, reporting every invalid token and duplicate
    /// custom property name.
    pub(super) fn parse(source: &str) -> Result<Self, Vec<String>> {
        let root = match serde_json::from_str::<Json>(source) {
            Ok(Json::Object(root)) => root,
            Ok(_) => return Err(vec!["expected a group at the root".to_owned()]),
            Err(error) => return Err(vec![format!("invalid JSON: {error}")]),
        };
        let mut parser = Parser::default();
        parser.group(&root, &Scope::default());
        let contrast = root
            .get("$extensions")
            .and_then(|extensions| extensions.get(CONTRAST_EXTENSION))
            .map_or(Ok(Vec::new()), |groups| {
                Vec::<ContrastGroup>::deserialize(groups)
            })
            .unwrap_or_else(|error| {
                parser.errors.push(format!("{CONTRAST_EXTENSION}: {error}"));

                Vec::new()
            });
        parser.check_names();
        if !parser.errors.is_empty() {
            return Err(parser.errors);
        }

        Ok(Self {
            contrast,
            tokens: parser.tokens,
        })
    }

    /// Returns the token at a dotted `path`, such as `color.bg`.
    pub(super) fn find(&self, path: &str) -> Option<&Token> {
        self.tokens.iter().find(|token| token.path == path)
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
    pub(super) name: String,
    pub(super) path: String,
    pub(super) surface: Option<String>,
    pub(super) value: Value,
}

impl Token {
    /// Returns the value in `theme`, falling back to the default.
    pub(super) fn value_in(&self, theme: &str) -> &Value {
        match &self.dark {
            Some(dark) if theme == DARK_THEME => dark,
            _ => &self.value,
        }
    }
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

/// Colour pairs that must keep a contrast ratio in every theme.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ContrastGroup {
    pub(super) backgrounds: Vec<String>,
    pub(super) foregrounds: Vec<String>,
    pub(super) minimum: f64,
    #[serde(default)]
    pub(super) shortfalls: Vec<Shortfall>,
    #[serde(rename = "use")]
    pub(super) usage: String,
}

/// A pair known to miss its group's minimum, with the measured ratio and why
/// it stays.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Shortfall {
    pub(super) background: String,
    pub(super) foreground: String,
    pub(super) ratio: f64,
    pub(super) reason: String,
    pub(super) theme: String,
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
        match (value, dark) {
            (Ok(value), Ok(dark)) => self.tokens.push(Token {
                dark,
                name: scope.name.join("-"),
                path: scope.path.join("."),
                surface: scope.surface.clone(),
                value,
            }),
            (value, dark) => {
                for error in [value.err(), dark.err()].into_iter().flatten() {
                    self.error(scope, &error);
                }
            }
        }
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
