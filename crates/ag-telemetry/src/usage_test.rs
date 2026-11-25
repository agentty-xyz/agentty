//! Cost attribute tests: charge and API-equivalent keys stay apart.

use std::collections::BTreeMap;

use super::{CostAmount, cost_attributes};

fn attribute_map(
    charge: Option<CostAmount>,
    api_equivalent: Option<CostAmount>,
    login: Option<&'static str>,
) -> BTreeMap<String, String> {
    cost_attributes(charge, api_equivalent, login)
        .into_iter()
        .map(|attribute| (attribute.key.to_string(), attribute.value.to_string()))
        .collect()
}

#[test]
fn cost_attributes_keep_charges_and_api_equivalents_apart() {
    // Arrange
    let charge = CostAmount {
        basis: "list",
        micro_usd: 250,
        upper_bound: false,
    };
    let api_equivalent = CostAmount {
        basis: "list",
        micro_usd: u64::MAX,
        upper_bound: true,
    };
    let pairs = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    let saturated = i64::MAX.to_string();
    let saturated = saturated.as_str();
    let cases = [
        ((None, None, Some("claude")), pairs(&[])),
        (
            (Some(charge), None, Some("claude")),
            pairs(&[
                ("agentty.usage.cost.basis", "list"),
                ("agentty.usage.cost.micro_usd", "250"),
                ("agentty.usage.cost.upper_bound", "false"),
            ]),
        ),
        (
            (Some(charge), Some(api_equivalent), Some("claude")),
            pairs(&[
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.login", "claude"),
                ("agentty.usage.api_equivalent.micro_usd", saturated),
                ("agentty.usage.api_equivalent.upper_bound", "true"),
                ("agentty.usage.cost.basis", "list"),
                ("agentty.usage.cost.micro_usd", "250"),
                ("agentty.usage.cost.upper_bound", "false"),
            ]),
        ),
        (
            (None, Some(api_equivalent), None),
            pairs(&[
                ("agentty.usage.api_equivalent.basis", "list"),
                ("agentty.usage.api_equivalent.micro_usd", saturated),
                ("agentty.usage.api_equivalent.upper_bound", "true"),
            ]),
        ),
    ];

    for ((charge, api_equivalent, login), expected) in cases {
        // Act
        let attributes = attribute_map(charge, api_equivalent, login);

        // Assert
        assert_eq!(attributes, expected);
    }
}
