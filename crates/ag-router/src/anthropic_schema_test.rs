//! Tests projecting output schemas onto the Anthropic structured-output
//! subset.

use serde_json::json;

use super::wire_schema;

#[test]
fn supported_schema_travels_unchanged() {
    // Arrange
    let schema = json!({
        "type": "object",
        "title": "Person",
        "description": "One person.",
        "properties": {
            "born": {"type": "string", "format": "date"},
            "code": {"type": "string", "pattern": "^[A-Z]+$"},
            "kind": {"type": "string", "enum": ["a", "b"], "default": "a"},
            "tags": {"type": "array", "minItems": 1, "items": {"type": "string"}},
            "version": {"const": 1},
            "parent": {"$ref": "#/$defs/person"}
        },
        "required": ["born"],
        "additionalProperties": false,
        "$defs": {
            "person": {"type": ["object", "null"], "properties": {}, "additionalProperties": false}
        }
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(wire.to_string(), schema.to_string());
}

#[test]
fn unsupported_constraints_move_into_descriptions_at_every_depth() {
    // Arrange
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["context", "decisions", "score"],
        "properties": {
            "context": {"type": "string", "minLength": 1, "maxLength": 4000},
            "decisions": {
                "type": "array",
                "description": "Decisions made.",
                "maxItems": 32,
                "minItems": 2,
                "uniqueItems": true,
                "items": {"type": "string", "minLength": 1, "maxLength": 400}
            },
            "score": {
                "type": "integer",
                "minimum": 0,
                "maximum": 10,
                "exclusiveMinimum": -1,
                "multipleOf": 2
            },
            "nested": {
                "anyOf": [
                    {"type": "number", "exclusiveMaximum": 1.5},
                    {"allOf": [{"$ref": "#/definitions/link"}]}
                ]
            }
        },
        "definitions": {
            "link": {"type": "string", "format": "iri", "not": {"const": ""}}
        }
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(
        wire,
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["context", "decisions", "score"],
            "properties": {
                "context": {"type": "string", "description": "{minLength: 1, maxLength: 4000}"},
                "decisions": {
                    "type": "array",
                    "description": "Decisions made.\n\n{maxItems: 32, minItems: 2, uniqueItems: true}",
                    "items": {"type": "string", "description": "{minLength: 1, maxLength: 400}"}
                },
                "score": {
                    "type": "integer",
                    "description":
                        "{minimum: 0, maximum: 10, exclusiveMinimum: -1, multipleOf: 2}"
                },
                "nested": {
                    "anyOf": [
                        {"type": "number", "description": "{exclusiveMaximum: 1.5}"},
                        {"allOf": [{"$ref": "#/definitions/link"}]}
                    ]
                }
            },
            "definitions": {
                "link": {"type": "string", "description": "{format: \"iri\", not: {\"const\":\"\"}}"}
            }
        })
    );
}

#[test]
fn one_of_becomes_any_of_unless_any_of_is_present() {
    // Arrange
    let schema = json!({
        "properties": {
            "alone": {"oneOf": [{"type": "string", "maxLength": 3}, {"type": "null"}]},
            "beside": {
                "anyOf": [{"type": "string"}],
                "oneOf": [{"type": "null"}]
            }
        }
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(
        wire,
        json!({
            "properties": {
                "alone": {
                    "anyOf": [{"type": "string", "description": "{maxLength: 3}"}, {"type": "null"}]
                },
                "beside": {
                    "anyOf": [{"type": "string"}],
                    "description": "{oneOf: [{\"type\":\"null\"}]}"
                }
            },
            "additionalProperties": false
        })
    );
}

#[test]
fn every_object_is_closed_and_open_objects_note_their_extra_keys() {
    // Arrange
    let schema = json!({
        "type": "object",
        "properties": {
            "open": {"type": "object", "additionalProperties": true},
            "map": {"type": "object", "additionalProperties": {"type": "string"}},
            "unstated": {"type": ["object", "null"]}
        }
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(
        wire,
        json!({
            "type": "object",
            "properties": {
                "open": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "{additionalProperties: true}"
                },
                "map": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "{additionalProperties: {\"type\":\"string\"}}"
                },
                "unstated": {"type": ["object", "null"], "additionalProperties": false}
            },
            "additionalProperties": false
        })
    );
}

#[test]
fn metadata_is_dropped_and_boolean_schemas_pass_through() {
    // Arrange
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "urn:example:list",
        "$comment": "internal",
        "type": "array",
        "minItems": 0,
        "items": true
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(wire, json!({"type": "array", "minItems": 0, "items": true}));
}

#[test]
fn property_named_like_a_keyword_is_still_a_property() {
    // Arrange
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "maxItems": {"type": "integer", "maximum": 5},
            "properties": {"type": "string"}
        }
    });

    // Act
    let wire = wire_schema(&schema);

    // Assert
    assert_eq!(
        wire,
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "maxItems": {"type": "integer", "description": "{maximum: 5}"},
                "properties": {"type": "string"}
            }
        })
    );
}
