use std::num::NonZeroUsize;

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{StoredTurnOptions, StoredTurnOptionsError};
use crate::{ComparisonBase, OutputSchema, Tool, ToolPolicy, TurnLimits, TurnOptions};

#[test]
fn snapshots_round_trip_and_reject_unknown_or_invalid_configuration() {
    // Arrange
    let options = TurnOptions::new(
        schema(),
        ToolPolicy::default().allow(Tool::Write),
        TurnLimits::new(NonZeroUsize::new(3).expect("nonzero budget")),
    );
    let encoded = StoredTurnOptions::encode(&options);
    let snapshot: Value = serde_json::from_str(&encoded).expect("snapshot JSON");
    let mut invalid = Vec::new();
    for (key, value) in [
        ("version", json!(5)),
        ("max_tool_calls", json!(0)),
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
    assert!(decoded.continuation_compatible(&options));
    assert_eq!(decoded.max_tool_calls, options.limits().max_tool_calls());
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
    let budget = TurnOptions::new(
        plain.schema().clone(),
        plain.tool_policy(),
        TurnLimits::new(NonZeroUsize::new(17).expect("budget")),
    );

    // Act
    let snapshots: Vec<_> = [&plain, &selected, &other_scope, &budget]
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
    assert!(snapshots[1].continuation_compatible(&selected));
    assert!(!snapshots[1].continuation_compatible(&plain));
    assert!(!snapshots[1].continuation_compatible(&other_scope));
    assert!(snapshots[0].continuation_compatible(&budget));
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
                TurnLimits::new(NonZeroUsize::new(8).expect("budget")),
            )
        })
        .collect();

    // Act
    let snapshots: Vec<_> = options
        .iter()
        .map(|options| {
            StoredTurnOptions::decode(&StoredTurnOptions::encode(options)).expect("snapshot")
        })
        .collect();
    let mut reordered: Value =
        serde_json::from_str(&StoredTurnOptions::encode(&options[0])).expect("snapshot JSON");
    reordered["output_schema"] = options[1].schema().value().clone();
    let restored = StoredTurnOptions::decode(&reordered.to_string()).expect("reordered snapshot");

    // Assert
    assert_eq!(snapshots[0].version, 3);
    assert_eq!(
        snapshots[0].fingerprint(),
        "ff322e5ad3b6da9df9bcead8ddd1d5f58157a4351f0a1c52756b67f29e1c71d4"
    );
    assert_eq!(snapshots[0].fingerprint(), snapshots[1].fingerprint());
    assert_ne!(snapshots[0].fingerprint(), snapshots[2].fingerprint());
    assert!(restored.continuation_compatible(&options[0]));
    assert!(!restored.continuation_compatible(&options[2]));
}

#[test]
fn version_one_options_remain_readable_but_never_imply_a_known_base() {
    // Arrange
    let options = turn_options();
    let legacy = json!({"version":1, "output_schema":options.schema().value(), "tool_policy":options.tool_policy(), "max_tool_calls":options.limits().max_tool_calls()});
    let mut conflicting = legacy.clone();
    conflicting["comparison_base"] = json!(ComparisonBase::fixture("repo").identity());

    // Act
    let stored = StoredTurnOptions::decode(&legacy.to_string()).expect("legacy options");
    let invalid = StoredTurnOptions::decode(&conflicting.to_string());

    // Assert
    assert!(stored.comparison_base.is_none());
    assert!(!stored.continuation_compatible(&options));
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
fn continuation_compares_schema_permissions_and_identity_for_both_known_versions() {
    // Arrange
    let selected =
        turn_options().with_comparison_base(ComparisonBase::fixture("missing-repository"));
    let budget = TurnOptions::new(
        selected.schema().clone(),
        selected.tool_policy(),
        TurnLimits::new(NonZeroUsize::new(17).expect("budget")),
    )
    .with_comparison_base(selected.comparison_base().expect("base").clone());
    let changed_schema = TurnOptions::new(
        OutputSchema::new(json!({"type": "string"})).expect("schema"),
        selected.tool_policy(),
        selected.limits(),
    )
    .with_comparison_base(selected.comparison_base().expect("base").clone());
    let changed_permissions = TurnOptions::new(
        selected.schema().clone(),
        selected.tool_policy().allow(Tool::Write),
        selected.limits(),
    )
    .with_comparison_base(selected.comparison_base().expect("base").clone());
    let changed_root =
        turn_options().with_comparison_base(ComparisonBase::fixture("other-repository"));

    // Act / Assert
    for version in [2, 3] {
        let snapshot = versioned_snapshot(&selected, version);
        let stored = StoredTurnOptions::decode(&snapshot.to_string()).expect("snapshot");
        let changed_budget = versioned_snapshot(&budget, version);
        assert_ne!(snapshot["fingerprint"], changed_budget["fingerprint"]);
        assert!(stored.continuation_compatible(&selected));
        assert!(stored.continuation_compatible(&budget));
        for incompatible in [
            &changed_schema,
            &changed_permissions,
            &changed_root,
            &turn_options(),
        ] {
            assert!(!stored.continuation_compatible(incompatible));
        }
        let mut changed_oid = snapshot;
        changed_oid["comparison_base"]["oid"] = json!("2".repeat(40));
        sign_snapshot(&mut changed_oid);
        let stored =
            StoredTurnOptions::decode(&changed_oid.to_string()).expect("historical identity");
        assert!(!stored.continuation_compatible(&selected));
    }
}

#[test]
fn historical_comparison_metadata_is_validated_without_a_live_repository() {
    // Arrange
    let selected =
        turn_options().with_comparison_base(ComparisonBase::fixture("missing-repository"));

    // Act / Assert
    for version in [2, 3] {
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
    for version in [1, 2, 3] {
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

fn versioned_snapshot(options: &TurnOptions, version: u8) -> Value {
    let mut snapshot: Value =
        serde_json::from_str(&StoredTurnOptions::encode(options)).expect("snapshot");
    snapshot["version"] = json!(version);
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
    TurnOptions::new(schema(), ToolPolicy::default(), TurnLimits::default())
}
