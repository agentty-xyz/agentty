//! Usage accumulation tests: known-only-when-every-request-reported details,
//! cost bases, context fill, and per-model ordering.

use crate::usage::{
    ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage, UsageCheckpoint, UsagePurpose,
};

/// Returns fully detailed usage of `model` for one request.
fn detailed(model: &str, input_tokens: u64, micro_usd: u64) -> ModelUsage {
    ModelUsage {
        cache_read_tokens: Some(input_tokens / 2),
        cache_write_tokens: Some(1),
        context: Some(ContextFill::of_request(input_tokens, Some(1_000))),
        cost: Some(CostEstimate {
            basis: CostBasis::ListPrice,
            micro_usd,
        }),
        input_tokens,
        output_tokens: 10,
        reasoning_tokens: Some(4),
        requests: 1,
        ..ModelUsage::new(model)
    }
}

#[test]
fn labels_are_stable() {
    // Arrange, Act
    let purposes = [
        UsagePurpose::Turn,
        UsagePurpose::Title,
        UsagePurpose::CommitAssist,
        UsagePurpose::Review,
        UsagePurpose::Rebase,
        UsagePurpose::Other,
    ]
    .map(UsagePurpose::as_str);
    let bases = [
        CostBasis::ListPrice,
        CostBasis::HostPrice,
        CostBasis::AgentReported,
        CostBasis::Mixed,
    ]
    .map(CostBasis::as_str);

    // Assert
    assert_eq!(
        purposes,
        [
            "turn",
            "title",
            "commit_assist",
            "review",
            "rebase",
            "other"
        ]
    );
    assert_eq!(bases, ["list", "host", "agent_reported", "mixed"]);
}

#[test]
fn adding_detailed_usage_sums_every_count() {
    // Arrange
    let mut usage = detailed("model", 100, 7);

    // Act
    usage.add(&detailed("model", 60, 5));

    // Assert
    assert_eq!(
        usage,
        ModelUsage {
            cache_read_tokens: Some(80),
            cache_write_tokens: Some(2),
            context: Some(ContextFill {
                last_input_tokens: 60,
                peak_input_tokens: 100,
                window_tokens: Some(1_000),
            }),
            cost: Some(CostEstimate {
                basis: CostBasis::ListPrice,
                micro_usd: 12,
            }),
            input_tokens: 160,
            output_tokens: 20,
            reasoning_tokens: Some(8),
            requests: 2,
            ..ModelUsage::new("model")
        }
    );
}

#[test]
fn an_unreported_detail_makes_the_sum_unknown() {
    // Arrange
    let mut usage = detailed("model", 100, 7);

    // Act
    usage.add(&ModelUsage::unreported("model", 1));

    // Assert
    assert_eq!(usage.cache_read_tokens, None);
    assert_eq!(usage.cache_write_tokens, None);
    assert_eq!(usage.reasoning_tokens, None);
    assert_eq!(usage.cost, None);
    assert_eq!(usage.context, detailed("model", 100, 7).context);
    assert_eq!((usage.requests, usage.unreported_requests), (2, 1));
    assert!(!usage.is_empty());
}

#[test]
fn a_context_reported_only_later_is_kept() {
    // Arrange
    let mut usage = ModelUsage::unreported("model", 1);

    // Act
    usage.add(&detailed("model", 50, 1));

    // Assert
    assert_eq!(
        usage.context,
        Some(ContextFill::of_request(50, Some(1_000)))
    );
}

#[test]
fn costs_from_different_bases_sum_to_mixed() {
    // Arrange
    let list = CostEstimate {
        basis: CostBasis::ListPrice,
        micro_usd: u64::MAX,
    };
    let reported = CostEstimate {
        basis: CostBasis::AgentReported,
        micro_usd: 3,
    };

    // Act
    let total = list.saturating_add(reported);

    // Assert
    assert_eq!(
        total,
        CostEstimate {
            basis: CostBasis::Mixed,
            micro_usd: u64::MAX,
        }
    );
}

#[test]
fn turn_usage_orders_models_by_latest_request() {
    // Arrange
    let mut usage = TurnUsage::single(detailed("main", 100, 1));
    usage.record(&detailed("summary", 40, 1));

    // Act
    usage.merge(&TurnUsage::single(detailed("main", 120, 1)));

    // Assert
    let models = usage
        .models
        .iter()
        .map(|usage| usage.model.as_str())
        .collect::<Vec<_>>();
    assert_eq!(models, ["summary", "main"]);
    assert_eq!(usage.input_tokens(), 260);
    assert_eq!(usage.output_tokens(), 30);
    assert_eq!(
        usage.context().map(|context| context.last_input_tokens),
        Some(120)
    );
}

#[test]
fn merging_keeps_the_latest_checkpoint() {
    // Arrange
    let cases = [
        (None, None, None),
        (Some("earlier"), None, Some("earlier")),
        (None, Some("later"), Some("later")),
        (Some("earlier"), Some("later"), Some("later")),
    ];

    for (current, next, expected) in cases {
        let checkpoint = |totals: &str| UsageCheckpoint::new("/worktree", totals);
        let mut usage = TurnUsage {
            checkpoint: current.map(checkpoint),
            ..TurnUsage::default()
        };
        let later = TurnUsage {
            checkpoint: next.map(checkpoint),
            ..TurnUsage::default()
        };

        // Act
        usage.merge(&later);

        // Assert
        assert_eq!(usage.checkpoint, expected.map(checkpoint));
    }
}

#[test]
fn empty_usage_records_nothing() {
    // Arrange, Act
    let usage = TurnUsage::default();

    // Assert
    assert_eq!(usage.context(), None);
    assert_eq!(usage.input_tokens(), 0);
    assert!(ModelUsage::new("model").is_empty());
}
