use serde_json::json;

use crate::session_model::{RecordedModel, next_generation};

#[test]
fn snapshot_rejects_incomplete_or_invalid_identity() {
    // Arrange
    let valid =
        json!({"generation":0,"key":"key","revision":"1","provider":"provider","model":"model"});
    let mut invalid = vec!["not json".to_string()];
    for (field, value) in [
        ("generation", json!(-1)),
        ("key", json!(null)),
        ("revision", json!(null)),
        ("key", json!("")),
        ("provider", json!(null)),
        ("model", json!(null)),
        ("provider", json!("")),
        ("model", json!(" ")),
    ] {
        let mut snapshot = valid.clone();
        snapshot[field] = value;
        invalid.push(snapshot.to_string());
    }

    // Act / Assert
    assert!(RecordedModel::decode(&valid.to_string()).is_ok());
    assert!(
        RecordedModel::decode(
            r#"{"generation":0,"key":null,"revision":null,"provider":null,"model":null}"#
        )
        .is_ok()
    );
    for value in invalid {
        assert!(RecordedModel::decode(&value).is_err(), "{value}");
    }
    assert!(next_generation(i64::MAX).is_err());
}
