//! TypeScript declarations rendered from the protocol JSON Schema.
//!
//! The renderer reads only the schema, so the declarations cannot say
//! anything the schema does not. It understands exactly the keywords the
//! schema uses and fails on any other, so a construct it cannot render
//! faithfully stops the drift check instead of producing a loose type.

use serde_json::{Map, Value};

/// Keywords the renderer reads.
const RENDERED_KEYWORDS: [&str; 9] = [
    "$ref",
    "additionalProperties",
    "anyOf",
    "const",
    "items",
    "oneOf",
    "properties",
    "required",
    "type",
];
/// Keywords that only annotate or constrain a value TypeScript cannot
/// narrow, such as an integer's width.
const IGNORED_KEYWORDS: [&str; 4] = ["default", "description", "format", "minimum"];

/// Renders the module of TypeScript declarations for `schema`: a header
/// naming `regenerate`, the protocol version as the `ProtocolVersion`
/// literal type, and one exported type per definition, in definition
/// order. The module declares types only, so it has no code to run.
///
/// # Errors
/// Returns the path and keyword of the first schema construct it cannot
/// render.
pub(super) fn render_module(schema: &Value, regenerate: &str) -> Result<String, String> {
    let version = schema
        .get("x-protocol-version")
        .and_then(Value::as_u64)
        .ok_or("the schema names no `x-protocol-version`")?;
    let definitions = schema
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or("the schema has no `$defs`")?;
    let mut lines = vec![
        "// Generated from the `ag-remote` protocol types; do not edit. Regenerate with:"
            .to_string(),
        format!("//   {regenerate}"),
        String::new(),
    ];
    lines.extend(doc_comment(schema, ""));
    lines.push(format!("export type ProtocolVersion = {version};"));
    for (name, definition) in definitions {
        let rendered =
            render_definition(definition).map_err(|error| format!("`$defs/{name}`{error}"))?;
        lines.push(String::new());
        lines.extend(doc_comment(definition, ""));
        lines.push(format!("export type {name} ={rendered};"));
    }
    lines.push(String::new());

    Ok(lines.join("\n"))
}

/// Renders one definition after `=`: a union one variant per line, and
/// any other type on the same line.
///
/// A definition that is only a JSON primitive, such as a transparent
/// string newtype, would render as a redundant alias of that primitive,
/// so it fails; its type is inlined with `#[schemars(inline)]` instead.
fn render_definition(definition: &Value) -> Result<String, String> {
    let Some(variants) = union_variants(definition)? else {
        let is_primitive = definition
            .get("type")
            .is_some_and(|name| !["array", "object"].contains(&name.as_str().unwrap_or("object")))
            && definition.get("const").is_none();
        if is_primitive {
            return Err(
                ": a primitive-only definition; mark its type `#[schemars(inline)]`".to_string(),
            );
        }

        return Ok(format!(" {}", render_type(definition, "")?));
    };
    let mut lines = vec![String::new()];
    for (index, variant) in variants.iter().enumerate() {
        let rendered =
            render_type(variant, "    ").map_err(|error| format!("/oneOf/{index}{error}"))?;
        lines.extend(doc_comment(variant, "  "));
        lines.push(format!("  | {rendered}"));
    }

    Ok(lines.join("\n"))
}

/// Returns the variants of a schema that is a `oneOf` or `anyOf` union
/// besides annotations, and fails on a union beside any other keyword the
/// renderer reads, which the union's type would leave out.
fn union_variants(schema: &Value) -> Result<Option<&Vec<Value>>, String> {
    let object = schema_object(schema)?;
    let Some(union) = object.get("oneOf").or_else(|| object.get("anyOf")) else {
        return Ok(None);
    };
    let structural = RENDERED_KEYWORDS
        .iter()
        .filter(|keyword| !["anyOf", "oneOf"].contains(*keyword))
        .find(|keyword| object.contains_key(**keyword));
    if let Some(keyword) = structural {
        return Err(format!(": a union beside `{keyword}` is not supported"));
    }
    let variants = union.as_array().ok_or("/oneOf is not an array")?;

    Ok(Some(variants))
}

/// Renders `schema` as a TypeScript type whose continuation lines start
/// at `indent`.
fn render_type(schema: &Value, indent: &str) -> Result<String, String> {
    let object = schema_object(schema)?;
    if let Some(keyword) = object.keys().find(|keyword| {
        !RENDERED_KEYWORDS.contains(&keyword.as_str())
            && !IGNORED_KEYWORDS.contains(&keyword.as_str())
    }) {
        return Err(format!(": unsupported keyword `{keyword}`"));
    }
    if let Some(variants) = union_variants(schema)? {
        let rendered = variants
            .iter()
            .enumerate()
            .map(|(index, variant)| {
                render_type(variant, indent).map_err(|error| format!("/oneOf/{index}{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        return Ok(rendered.join(" | "));
    }
    if let Some(constant) = object.get("const") {
        return constant
            .as_str()
            .map(string_literal)
            .ok_or_else(|| ": only string `const` values are supported".to_string());
    }
    let reference = object.get("$ref").map(reference_name).transpose()?;
    if object.get("type").and_then(Value::as_str) == Some("object") {
        let literal = render_object(object, indent)?;

        return Ok(match reference {
            Some(reference) => format!("{literal} & {reference}"),
            None => literal,
        });
    }
    if let Some(reference) = reference {
        return Ok(reference);
    }

    render_primitive(object, indent)
}

/// Returns `schema` as an object, the only schema form the protocol uses.
fn schema_object(schema: &Value) -> Result<&Map<String, Value>, String> {
    schema
        .as_object()
        .ok_or_else(|| ": boolean schemas are not supported".to_string())
}

/// Returns the definition name a local `$ref` points at.
fn reference_name(reference: &Value) -> Result<String, String> {
    reference
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .map(str::to_string)
        .ok_or_else(|| ": only `#/$defs/` references are supported".to_string())
}

/// Renders an object schema as a type literal, one property per line, a
/// property outside `required` marked optional.
fn render_object(object: &Map<String, Value>, indent: &str) -> Result<String, String> {
    if object
        .get("additionalProperties")
        .is_some_and(|additional| additional != &Value::Bool(false))
    {
        return Err(": only `additionalProperties: false` is supported".to_string());
    }
    let empty = Map::new();
    let properties = match object.get("properties") {
        Some(properties) => properties
            .as_object()
            .ok_or("/properties is not an object")?,
        None => &empty,
    };
    let required: Vec<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |required| {
            required.iter().filter_map(Value::as_str).collect()
        });
    let inner = format!("{indent}  ");
    let mut lines = vec!["{".to_string()];
    for (name, property) in properties {
        let rendered =
            render_type(property, &inner).map_err(|error| format!("/properties/{name}{error}"))?;
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        lines.extend(doc_comment(property, &inner));
        lines.push(format!(
            "{inner}{}{optional}: {rendered};",
            property_name(name)
        ));
    }
    lines.push(format!("{indent}}}"));

    Ok(lines.join("\n"))
}

/// Renders a `type` schema: a JSON primitive, an array, or a union of
/// them such as `["string", "null"]`.
fn render_primitive(object: &Map<String, Value>, indent: &str) -> Result<String, String> {
    let names = match object.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => return Err(": a schema without `type`, `$ref`, `const`, or a union".to_string()),
    };
    let rendered = names
        .into_iter()
        .map(|name| match name {
            "array" => {
                let items = object.get("items").ok_or("/items is missing")?;
                let item = render_type(items, indent).map_err(|error| format!("/items{error}"))?;
                if item.contains(' ') {
                    Ok(format!("Array<{item}>"))
                } else {
                    Ok(format!("{item}[]"))
                }
            }
            "boolean" => Ok("boolean".to_string()),
            "integer" | "number" => Ok("number".to_string()),
            "null" => Ok("null".to_string()),
            "string" => Ok("string".to_string()),
            other => Err(format!(": unsupported type `{other}`")),
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(rendered.join(" | "))
}

/// Returns `name` as a property key: bare when it is an identifier,
/// quoted otherwise.
fn property_name(name: &str) -> String {
    let is_identifier = name
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    if is_identifier {
        return name.to_string();
    }

    string_literal(name)
}

/// Returns `text` as a double-quoted TypeScript string literal.
fn string_literal(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// Returns the `description` of `schema` as `JSDoc` comment lines at
/// `indent`: one line when it fits on one, none without a description.
fn doc_comment(schema: &Value, indent: &str) -> Vec<String> {
    let Some(text) = schema.get("description").and_then(Value::as_str) else {
        return Vec::new();
    };
    let text = text.replace("*/", "*\\/");
    let lines: Vec<&str> = text.lines().collect();
    if let [line] = lines.as_slice() {
        return vec![format!("{indent}/** {line} */")];
    }
    let mut comment = vec![format!("{indent}/**")];
    comment.extend(lines.into_iter().map(|line| {
        if line.is_empty() {
            format!("{indent} *")
        } else {
            format!("{indent} * {line}")
        }
    }));
    comment.push(format!("{indent} */"));

    comment
}
