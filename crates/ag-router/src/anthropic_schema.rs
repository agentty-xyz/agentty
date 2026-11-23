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
//! They also restrict combinators: an `anyOf` may sit only beside annotations,
//! and an `allOf` of two or more schemas may not directly hold `$ref`,
//! `anyOf`, or `allOf`.
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
///
/// The API also rejects an `anyOf` beside keywords other than annotations,
/// and an `allOf` of two or more schemas when one directly holds `$ref`,
/// `anyOf`, or `allOf`. Such an `allOf` is merged into its schema, and the
/// keywords beside such an `anyOf` move into each of its branches; when the
/// schemas conflict or a `$ref` is involved, the `allOf` or `anyOf` moves
/// into the description instead.
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
    if !flatten_all_of(&mut wire) {
        let all_of = wire.shift_remove("allOf").unwrap_or_default();
        moved.push(format!("allOf: {all_of}"));
    }
    if !distribute_any_of(&mut wire) {
        let any_of = wire.shift_remove("anyOf").unwrap_or_default();
        moved.push(format!("anyOf: {any_of}"));
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

/// Merges an `allOf` of two or more schemas that the API rejects, because
/// one of them directly holds `$ref`, `anyOf`, or `allOf`, into `wire`.
/// Returns `false`, leaving `wire` unchanged, when the schemas cannot be
/// merged: one holds `$ref` or `allOf`, two hold `anyOf`, or they set one
/// keyword differently.
fn flatten_all_of(wire: &mut Map<String, Value>) -> bool {
    let Some(branches) = wire.get("allOf").and_then(Value::as_array) else {
        return true;
    };
    let rejected = branches.len() > 1
        && branches.iter().any(|branch| {
            ["$ref", "anyOf", "allOf"]
                .iter()
                .any(|keyword| branch.get(keyword).is_some())
        });
    if !rejected {
        return true;
    }
    let mut flattened = wire.clone();
    flattened.shift_remove("allOf");
    for branch in branches {
        let Some(branch) = branch.as_object() else {
            return false;
        };
        if branch.contains_key("$ref") || branch.contains_key("allOf") {
            return false;
        }
        let Some(merged) = merged(&flattened, branch) else {
            return false;
        };
        flattened = merged;
    }
    *wire = flattened;

    true
}

/// Moves the keywords beside an `anyOf`, which the API accepts only beside
/// annotations, into each of its branches. Returns `false`, leaving `wire`
/// unchanged, when a branch is a `$ref` or not an object, or sets one of
/// those keywords differently.
fn distribute_any_of(wire: &mut Map<String, Value>) -> bool {
    const ANNOTATIONS: [&str; 6] = [
        "$defs",
        "anyOf",
        "default",
        "definitions",
        "description",
        "title",
    ];
    let Some(branches) = wire.get("anyOf").and_then(Value::as_array) else {
        return true;
    };
    let siblings: Map<String, Value> = wire
        .iter()
        .filter(|(keyword, _)| !ANNOTATIONS.contains(&keyword.as_str()))
        .map(|(keyword, value)| (keyword.clone(), value.clone()))
        .collect();
    if siblings.is_empty() {
        return true;
    }
    let mut distributed = Vec::new();
    for branch in branches {
        let Some(branch) = branch
            .as_object()
            .filter(|branch| !branch.contains_key("$ref"))
        else {
            return false;
        };
        let Some(merged) = merged(branch, &siblings) else {
            return false;
        };
        distributed.push(Value::Object(merged));
    }
    wire.retain(|keyword, _| ANNOTATIONS.contains(&keyword.as_str()));
    wire.insert("anyOf".to_string(), Value::Array(distributed));

    true
}

/// Returns `target` with the keywords of `source` added, or `None` when both
/// set one keyword differently. Both `required` lists are kept, both
/// `properties` maps unless they define one property differently, and the
/// `description` and `title` of `target`.
fn merged(target: &Map<String, Value>, source: &Map<String, Value>) -> Option<Map<String, Value>> {
    let mut merged = target.clone();
    for (keyword, value) in source {
        let Some(existing) = merged.get_mut(keyword) else {
            merged.insert(keyword.clone(), value.clone());
            continue;
        };
        if existing == value {
            continue;
        }
        match (keyword.as_str(), existing, value) {
            ("description" | "title", _, _) => {}
            ("required", Value::Array(names), Value::Array(added)) => {
                for name in added {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
            ("properties", Value::Object(properties), Value::Object(added)) => {
                for (name, schema) in added {
                    match properties.get(name) {
                        Some(existing) if existing != schema => return None,
                        _ => {
                            properties.insert(name.clone(), schema.clone());
                        }
                    }
                }
            }
            _ => return None,
        }
    }

    Some(merged)
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
