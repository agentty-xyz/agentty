use std::num::NonZeroUsize;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{StoredTurnOptions, StoredTurnOptionsError};
use crate::{ComparisonBase, OutputSchema, Tool, ToolPolicy, TurnOptions};

#[test]
fn snapshots_round_trip_and_reject_unknown_or_invalid_configuration() {
    // Arrange
    let options = TurnOptions::new(schema(), ToolPolicy::default().allow(Tool::Write));
    let encoded = StoredTurnOptions::encode(&options);
    let snapshot: Value = serde_json::from_str(&encoded).expect("snapshot JSON");
    let mut invalid = Vec::new();
    for (key, value) in [
        ("version", json!(6)),
        ("max_tool_calls", json!(0)),
        ("max_tool_calls", json!(8)),
        ("output_schema", json!({"type":"invalid"})),
        ("tool_policy", json!({"read":true})),
        ("unknown", json!(true)),
    ] {
        let mut value_snapshot = snapshot.clone();
        value_snapshot[key] = value;
        invalid.push(value_snapshot.to_string());
    }
    invalid.push("invalid JSON".to_string());

    // Act
    let decoded = StoredTurnOptions::decode(&encoded).expect("decode snapshot");
    let errors: Vec<_> = invalid
        .iter()
        .map(|snapshot| StoredTurnOptions::decode(snapshot))
        .collect();

    // Assert
    assert_eq!(decoded.output_schema, *options.schema().value());
    assert_eq!(decoded.tool_policy, options.tool_policy());
    assert_eq!(decoded.version, 5);
    assert_eq!(decoded.max_tool_calls, None);
    assert!(snapshot.get("max_tool_calls").is_none());
    assert!(errors.iter().all(Result::is_err));
}

#[test]
fn comparison_snapshots_preserve_identity_and_fingerprint_all_effective_options() {
    // Arrange
    let plain = turn_options();
    let selected = plain
        .clone()
        .with_comparison_base(ComparisonBase::fixture("deleted-repository"));
    let other_scope = plain
        .clone()
        .with_comparison_base(ComparisonBase::fixture("other-repository"));
    let permissions = TurnOptions::new(
        plain.schema().clone(),
        plain.tool_policy().allow(Tool::Read),
    );

    // Act
    let snapshots: Vec<_> = [&plain, &selected, &other_scope, &permissions]
        .into_iter()
        .map(|options| {
            StoredTurnOptions::decode(&StoredTurnOptions::encode(options)).expect("stored metadata")
        })
        .collect();
    let fingerprints: Vec<_> = snapshots
        .iter()
        .map(StoredTurnOptions::fingerprint)
        .collect();

    // Assert
    assert_eq!(
        snapshots[1].comparison_base.as_ref(),
        selected.comparison_base().map(ComparisonBase::identity)
    );
    assert!(snapshots[0].comparison_base.is_none());
    for (index, fingerprint) in fingerprints.iter().enumerate() {
        assert_eq!(fingerprint.len(), 64);
        assert!(!fingerprints[..index].contains(fingerprint));
    }
}

#[test]
fn version_three_fingerprints_sort_nested_object_keys_and_preserve_array_order() {
    // Arrange
    let schemas = [
        r#"{"type":"array","prefixItems":[{"type":"string","minLength":1},{"type":"integer","minimum":0}]}"#,
        r#"{"prefixItems":[{"minLength":1,"type":"string"},{"minimum":0,"type":"integer"}],"type":"array"}"#,
        r#"{"type":"array","prefixItems":[{"type":"integer","minimum":0},{"type":"string","minLength":1}]}"#,
    ];
    let options: Vec<_> = schemas
        .iter()
        .map(|schema| {
            TurnOptions::new(
                OutputSchema::new(serde_json::from_str(schema).expect("schema JSON"))
                    .expect("schema"),
                ToolPolicy::default(),
            )
        })
        .collect();

    // Act
    let snapshots: Vec<_> = options
        .iter()
        .map(|options| {
            StoredTurnOptions::decode(&versioned_snapshot(options, 3).to_string())
                .expect("snapshot")
        })
        .collect();
    let mut reordered = versioned_snapshot(&options[0], 3);
    reordered["output_schema"] = options[1].schema().value().clone();
    let restored = StoredTurnOptions::decode(&reordered.to_string()).expect("reordered snapshot");

    // Assert
    assert_eq!(snapshots[0].version, 3);
    assert_eq!(snapshots[0].max_tool_calls.map(NonZeroUsize::get), Some(8));
    assert_eq!(
        snapshots[0].fingerprint(),
        "ff322e5ad3b6da9df9bcead8ddd1d5f58157a4351f0a1c52756b67f29e1c71d4"
    );
    assert_eq!(snapshots[0].fingerprint(), snapshots[1].fingerprint());
    assert_ne!(snapshots[0].fingerprint(), snapshots[2].fingerprint());
    assert_eq!(restored.fingerprint(), snapshots[0].fingerprint());
}

#[test]
fn version_one_options_remain_readable_but_never_imply_a_known_base() {
    // Arrange
    let options = turn_options();
    let legacy = json!({"version":1, "output_schema":options.schema().value(), "tool_policy":options.tool_policy(), "max_tool_calls":8});
    let mut conflicting = legacy.clone();
    conflicting["comparison_base"] = json!(ComparisonBase::fixture("repo").identity());

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string()).expect("legacy options");
    let invalid = StoredTurnOptions::decode(&conflicting.to_string());

    // Assert
    assert!(stored.comparison_base.is_none());
    assert!(stored.fingerprint.is_none());
    assert!(invalid.is_err());
}

#[test]
fn comparison_metadata_and_fingerprint_corruption_are_rejected() {
    // Arrange
    let options = turn_options().with_comparison_base(ComparisonBase::fixture("repo"));
    let mut snapshot: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options)).expect("snapshot");
    snapshot["comparison_base"]["oid"] = json!("HEAD");
    let stored: StoredTurnOptions =
        serde_json::from_value(snapshot.clone()).expect("typed metadata");
    snapshot["fingerprint"] = json!(stored.fingerprint());
    let mut mismatched: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options)).expect("snapshot");
    mismatched["fingerprint"] = json!("incorrect");

    // Act / Assert
    assert!(StoredTurnOptions::decode(&snapshot.to_string()).is_err());
    assert!(StoredTurnOptions::decode(&mismatched.to_string()).is_err());
}
#[test]
fn version_two_fingerprints_retain_legacy_serialization() {
    // Arrange
    let schema = serde_json::from_str::<Value>(
        r#"{"type":"array","prefixItems":[{"type":"string","minLength":1},{"type":"integer","minimum":0}]}"#,
    )
    .expect("schema JSON");
    let mut legacy = json!({
        "comparison_base": null,
        "max_tool_calls": 8,
        "output_schema": schema,
        "tool_policy": ToolPolicy::default(),
        "version": 2,
    });
    let fingerprint = hex::encode(Sha256::digest(legacy.to_string()));
    legacy["fingerprint"] = json!(fingerprint);

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string()).expect("legacy snapshot");

    // Assert
    assert_eq!(stored.version, 2);
    assert_eq!(stored.output_schema, schema);
    assert_eq!(stored.fingerprint(), fingerprint);
}

#[test]
fn legacy_tool_call_limits_remain_part_of_the_recorded_fingerprint() {
    // Arrange
    let selected =
        turn_options().with_comparison_base(ComparisonBase::fixture("missing-repository"));

    // Act / Assert
    for version in [2, 3, 4] {
        let snapshot = versioned_snapshot(&selected, version);
        let mut changed_limit = snapshot.clone();
        changed_limit["max_tool_calls"] = json!(17);
        sign_snapshot(&mut changed_limit);
        let stored = StoredTurnOptions::decode(&changed_limit.to_string()).expect("legacy limit");
        assert_eq!(stored.max_tool_calls.map(NonZeroUsize::get), Some(17));
        assert_ne!(snapshot["fingerprint"], changed_limit["fingerprint"]);
    }
}

#[test]
fn historical_comparison_metadata_is_validated_without_a_live_repository() {
    // Arrange
    let selected =
        turn_options().with_comparison_base(ComparisonBase::fixture("missing-repository"));

    // Act / Assert
    for version in [2, 3, 5] {
        for oid in ["a".repeat(40), "A".repeat(64)] {
            let mut snapshot = versioned_snapshot(&selected, version);
            snapshot["comparison_base"]["oid"] = json!(oid);
            sign_snapshot(&mut snapshot);
            assert!(StoredTurnOptions::decode(&snapshot.to_string()).is_ok());
        }
        for (key, value) in [
            ("oid", json!("HEAD")),
            ("oid", json!("g".repeat(40))),
            ("repository_root", json!([])),
        ] {
            let mut snapshot = versioned_snapshot(&selected, version);
            snapshot["comparison_base"][key] = value;
            sign_snapshot(&mut snapshot);
            assert!(matches!(
                StoredTurnOptions::decode(&snapshot.to_string()),
                Err(StoredTurnOptionsError::InvalidData { .. })
            ));
        }
        let mut unknown = versioned_snapshot(&selected, version);
        unknown["comparison_base"]["unknown"] = json!(true);
        assert!(matches!(
            StoredTurnOptions::decode(&unknown.to_string()),
            Err(StoredTurnOptionsError::Json(_))
        ));
        for fingerprint in [Value::Null, json!("incorrect")] {
            let mut snapshot = versioned_snapshot(&selected, version);
            snapshot["fingerprint"] = fingerprint;
            assert!(matches!(
                StoredTurnOptions::decode(&snapshot.to_string()),
                Err(StoredTurnOptionsError::InvalidData { .. })
            ));
        }
    }
}

#[test]
fn every_version_validates_schemas_and_version_one_rejects_fingerprints() {
    // Arrange
    let options = turn_options();

    // Act / Assert
    for version in [1, 2, 3, 5] {
        let mut snapshot = versioned_snapshot(&options, version);
        snapshot["output_schema"] = json!({"type": "invalid"});
        assert!(matches!(
            StoredTurnOptions::decode(&snapshot.to_string()),
            Err(StoredTurnOptionsError::Schema(_))
        ));
    }
    let mut legacy = versioned_snapshot(&options, 1);
    legacy["fingerprint"] = json!("unexpected");
    assert!(matches!(
        StoredTurnOptions::decode(&legacy.to_string()),
        Err(StoredTurnOptionsError::InvalidData { .. })
    ));
}

#[test]
fn tool_call_limits_are_required_before_version_five_and_rejected_after() {
    // Arrange
    let options = turn_options();
    let mut legacy_without_limit = versioned_snapshot(&options, 3);
    legacy_without_limit
        .as_object_mut()
        .expect("object")
        .remove("max_tool_calls");
    sign_snapshot(&mut legacy_without_limit);
    let mut current_with_limit = versioned_snapshot(&options, 5);
    current_with_limit["max_tool_calls"] = json!(8);
    sign_snapshot(&mut current_with_limit);

    // Act
    let legacy = StoredTurnOptions::decode(&legacy_without_limit.to_string());
    let current = StoredTurnOptions::decode(&current_with_limit.to_string());

    // Assert
    assert!(matches!(
        legacy,
        Err(StoredTurnOptionsError::InvalidData { .. })
    ));
    assert!(matches!(
        current,
        Err(StoredTurnOptionsError::InvalidData { .. })
    ));
}

#[test]
fn version_five_fingerprints_sort_object_keys_without_a_tool_call_limit() {
    // Arrange
    let ordered =
        OutputSchema::new(json!({"type": "object", "required": ["name"]})).expect("ordered schema");
    let reordered = OutputSchema::new(
        serde_json::from_str(r#"{"required":["name"],"type":"object"}"#).expect("schema JSON"),
    )
    .expect("reordered schema");

    // Act
    let snapshots: Vec<_> = [ordered, reordered]
        .into_iter()
        .map(|schema| {
            let options = TurnOptions::new(schema, ToolPolicy::default());

            StoredTurnOptions::decode(&StoredTurnOptions::encode(&options)).expect("snapshot")
        })
        .collect();

    // Assert
    assert_eq!(snapshots[0].version, 5);
    assert!(
        !snapshots[0]
            .effective_options()
            .as_object()
            .expect("object")
            .contains_key("max_tool_calls")
    );
    assert_eq!(snapshots[0].fingerprint(), snapshots[1].fingerprint());
}

/// Rewrites a current snapshot as `version`, adding the default legacy
/// tool-call limit and dropping fields that version did not record.
fn versioned_snapshot(options: &TurnOptions, version: u8) -> Value {
    let mut snapshot: Value =
        serde_json::from_str(&StoredTurnOptions::encode(options)).expect("snapshot");
    snapshot["version"] = json!(version);
    if version < 5 {
        snapshot["max_tool_calls"] = json!(8);
    }
    if version < 4 {
        snapshot.as_object_mut().expect("object").remove("bash");
    }
    if version == 1 {
        snapshot
            .as_object_mut()
            .expect("object")
            .remove("comparison_base");
        snapshot
            .as_object_mut()
            .expect("object")
            .remove("fingerprint");
    } else {
        sign_snapshot(&mut snapshot);
    }

    snapshot
}

fn sign_snapshot(snapshot: &mut Value) {
    let stored: StoredTurnOptions =
        serde_json::from_value(snapshot.clone()).expect("typed metadata");
    snapshot["fingerprint"] = json!(stored.fingerprint());
}

fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type": "object"})).expect("schema")
}

fn turn_options() -> TurnOptions {
    TurnOptions::new(schema(), ToolPolicy::default())
}
