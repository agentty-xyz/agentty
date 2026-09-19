use super::json_prefix;

#[test]
fn json_prefix_maximizes_encoded_budget_without_splitting_utf8() {
    // Arrange
    let values = [
        "",
        "ASCII steering",
        "界🦀é",
        "\0\n\r\t\"\\",
        "prefix\n界🦀end",
    ];
    for value in values {
        for budget in 2..=40 {
            // Act
            let prefix = json_prefix(value, budget);

            // Assert
            assert!(value.starts_with(prefix));
            assert!(serde_json::json!(prefix).to_string().len() <= budget);
            if let Some(next) = value[prefix.len()..].chars().next() {
                let longer = &value[..prefix.len() + next.len_utf8()];
                assert!(serde_json::json!(longer).to_string().len() > budget);
            }
        }
    }
}
