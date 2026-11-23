//! Projection of a provider-neutral output schema onto the JSON Schema subset
//! that Anthropic structured outputs accept.
//!
//! The Messages API compiles `output_config.format.schema` into a grammar and
//! answers a schema using a keyword outside its documented subset with a
//! `400`. The documented limits
//! (<https://platform.claude.com/docs/en/build-with-claude/structured-outputs#json-schema-limitations>)
//! exclude numeric bounds (`minimum`, `maximum`, `multipleOf`), string lengths
//! (`minLength`, `maxLength`), array bounds other than a `minItems` of 0 or 1,
//! `oneOf`, `not`, string formats outside a fixed list, and
//! `additionalProperties` other than `false`, which every object must set.
//!
//! [`wire_schema`] follows the transformation the official SDKs document: it
//! keeps each supported keyword, rewrites `oneOf` as `anyOf`, closes every
//! object with `additionalProperties: false`, and moves every other keyword
//! into its schema's `description` as `{keyword: value}`, so the model still
//! reads the constraint. The caller keeps validating responses against the
//! original schema, so every moved constraint is still enforced locally.

use serde_json::{Map, Value};

/// Keywords the API accepts unchanged.
const KEPT_KEYWORDS: [&str; 9] = [
    "$ref",
    "const",
    "default",
    "description",
    "enum",
    "pattern",
    "required",
    "title",
    "type",
];
/// Keywords whose value maps names to subschemas.
const SCHEMA_MAP_KEYWORDS: [&str; 3] = ["$defs", "definitions", "properties"];
/// Keywords whose value lists subschemas.
const SCHEMA_LIST_KEYWORDS: [&str; 2] = ["allOf", "anyOf"];
/// Document metadata that constrains nothing and is dropped.
const DROPPED_KEYWORDS: [&str; 3] = ["$comment", "$id", "$schema"];
/// String formats the API supports.
const SUPPORTED_FORMATS: [&str; 10] = [
    "date",
    "date-time",
    "duration",
    "email",
    "hostname",
    "ipv4",
    "ipv6",
    "time",
    "uri",
    "uuid",
];

/// Returns the schema to send for `schema`, which stays unchanged.
///
/// Subschemas under `properties`, `$defs`, `definitions`, `items`, `anyOf`,
/// `allOf`, and `oneOf` are projected the same way. A `oneOf` beside an
/// `anyOf`, and subschemas under unsupported keywords such as `not`, move
/// into the description whole.
pub(crate) fn wire_schema(schema: &Value) -> Value {
    let Value::Object(object) = schema else {
        return schema.clone();
    };
    let mut wire = Map::new();
    let mut moved = Vec::new();
    for (keyword, value) in object {
        let keyword = keyword.as_str();
        if KEPT_KEYWORDS.contains(&keyword) || is_supported_value(keyword, value) {
            wire.insert(keyword.to_string(), value.clone());
        } else if SCHEMA_MAP_KEYWORDS.contains(&keyword) && value.is_object() {
            wire.insert(keyword.to_string(), schema_map(value));
        } else if SCHEMA_LIST_KEYWORDS.contains(&keyword) && value.is_array() {
            wire.insert(keyword.to_string(), schema_list(value));
        } else if keyword == "oneOf" && value.is_array() && !object.contains_key("anyOf") {
            wire.insert("anyOf".to_string(), schema_list(value));
        } else if keyword == "items" {
            wire.insert(keyword.to_string(), wire_schema(value));
        } else if !DROPPED_KEYWORDS.contains(&keyword) {
            moved.push(format!("{keyword}: {value}"));
        }
    }
    if is_object_schema(object) {
        wire.insert("additionalProperties".to_string(), Value::Bool(false));
    }
    if !moved.is_empty() {
        let note = format!("{{{}}}", moved.join(", "));
        let description = match wire.get("description").and_then(Value::as_str) {
            Some(description) => format!("{description}\n\n{note}"),
            None => note,
        };
        wire.insert("description".to_string(), Value::String(description));
    }

    Value::Object(wire)
}

/// Returns whether `value` is a supported value of a keyword the API accepts
/// only with some values.
fn is_supported_value(keyword: &str, value: &Value) -> bool {
    match keyword {
        "additionalProperties" => value == &Value::Bool(false),
        "format" => value
            .as_str()
            .is_some_and(|format| SUPPORTED_FORMATS.contains(&format)),
        "minItems" => value.as_u64().is_some_and(|items| items <= 1),
        _ => false,
    }
}

/// Projects every subschema of a name-to-schema map.
fn schema_map(value: &Value) -> Value {
    value
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, schema)| (name.clone(), wire_schema(schema)))
        .collect::<Map<_, _>>()
        .into()
}

/// Projects every subschema of a list.
fn schema_list(value: &Value) -> Value {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(wire_schema)
        .collect()
}

/// Returns whether `object` describes an object: its `type` is or includes
/// `object`, or it declares `properties`.
fn is_object_schema(object: &Map<String, Value>) -> bool {
    let declares_object = object.get("type").is_some_and(|schema_type| {
        schema_type == "object"
            || schema_type
                .as_array()
                .is_some_and(|types| types.iter().any(|schema_type| schema_type == "object"))
    });

    declares_object || object.contains_key("properties")
}

#[cfg(test)]
#[path = "anthropic_schema_test.rs"]
mod tests;
