//! Session usage accumulation, live folding, and label parsing tests.

use ag_contracts::{ContextFill, CostBasis, CostEstimate, ModelUsage, TurnUsage, UsagePurpose};

use super::{
    ContextNow, LiveUsageRun, SessionUsage, UsageRecord, parse_cost_basis, parse_usage_purpose,
};

/// Returns one detailed request of `model` with `input_tokens` of input.
fn detailed_usage(model: &str, input_tokens: u64, cost: Option<CostEstimate>) -> ModelUsage {
    ModelUsage {
        cache_read_tokens: Some(input_tokens / 2),
        cache_write_tokens: Some(1),
        context: Some(ContextFill::of_request(input_tokens, Some(1_000))),
        cost,
        input_tokens,
        output_tokens: 10,
        reasoning_tokens: Some(2),
        requests: 1,
        ..ModelUsage::new(model)
    }
}

/// Returns a list-price estimate of `micro_usd`.
fn list_price(micro_usd: u64) -> CostEstimate {
    CostEstimate {
        basis: CostBasis::ListPrice,
        micro_usd,
    }
}

#[test]
fn test_add_keeps_details_known_only_when_both_sides_know_them() {
    // Arrange
    let mut record = UsageRecord::from_model_usage(
        UsagePurpose::Turn,
        &detailed_usage("model", 400, Some(list_price(70))),
    );
    let agent_reported = UsageRecord::from_model_usage(
        UsagePurpose::Turn,
        &detailed_usage(
            "model",
            200,
            Some(CostEstimate {
                basis: CostBasis::AgentReported,
                micro_usd: 30,
            }),
        ),
    );
    let unreported =
        UsageRecord::from_model_usage(UsagePurpose::Turn, &ModelUsage::unreported("model", 2));

    // Act
    record.add(&agent_reported);
    record.add(&unreported);

    // Assert
    assert_eq!(
        record,
        UsageRecord {
            cache_read_tokens: None,
            cache_write_tokens: None,
            context_peak_tokens: Some(400),
            context_window_tokens: Some(1_000),
            cost_basis: Some(CostBasis::Mixed),
            cost_micro_usd: Some(100),
            detailed_invocation_count: 3,
            input_tokens: 600,
            invocation_count: 3,
            model: "model".to_string(),
            output_tokens: 20,
            purpose: UsagePurpose::Turn,
            reasoning_tokens: None,
            request_count: 4,
            unpriced_invocation_count: 1,
            unreported_request_count: 2,
        }
    );
    assert_eq!(record.undetailed_invocation_count(), 0);
    assert!(!record.is_fully_unreported());
}

#[test]
fn test_add_takes_the_known_side_of_cost_and_context() {
    // Arrange
    let mut record =
        UsageRecord::from_model_usage(UsagePurpose::Turn, &ModelUsage::unreported("model", 1));
    let priced = UsageRecord::from_model_usage(
        UsagePurpose::Turn,
        &detailed_usage("model", 300, Some(list_price(5))),
    );

    // Act
    record.add(&priced);

    // Assert
    assert_eq!(record.context_peak_tokens, Some(300));
    assert_eq!(record.cost_basis, Some(CostBasis::ListPrice));
    assert_eq!(record.cost_micro_usd, Some(5));
}

#[test]
fn test_is_fully_unreported_requires_every_request_unreported() {
    // Arrange
    let unreported =
        UsageRecord::from_model_usage(UsagePurpose::Turn, &ModelUsage::unreported("model", 2));
    let empty = UsageRecord::from_model_usage(UsagePurpose::Turn, &ModelUsage::new("model"));

    // Act
    let is_unreported = unreported.is_fully_unreported();
    let is_empty_unreported = empty.is_fully_unreported();

    // Assert
    assert!(is_unreported);
    assert!(!is_empty_unreported);
}

#[test]
fn test_with_live_folds_turn_usage_into_turn_records_and_takes_its_context() {
    // Arrange
    let stored = SessionUsage {
        context: Some(ContextNow {
            tokens: 50,
            window_tokens: Some(1_000),
        }),
        records: vec![
            UsageRecord::from_model_usage(
                UsagePurpose::Turn,
                &detailed_usage("alpha", 100, Some(list_price(10))),
            ),
            UsageRecord::from_model_usage(
                UsagePurpose::Title,
                &detailed_usage("alpha", 20, Some(list_price(1))),
            ),
        ],
        turn_count: 1,
    };
    let mut live = TurnUsage::single(detailed_usage("alpha", 300, Some(list_price(20))));
    live.record(&detailed_usage("beta", 600, None));

    // Act
    let rebase = TurnUsage::single(detailed_usage("alpha", 50, None));
    let folded = stored.with_live([
        (&LiveUsageRun::SessionTurn, &live),
        (&LiveUsageRun::Rebase, &rebase),
    ]);
    let unchanged = stored.with_live([]);

    // Assert
    let labels = folded
        .records
        .iter()
        .map(|record| (record.model.as_str(), record.purpose))
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        [
            ("alpha", UsagePurpose::Turn),
            ("alpha", UsagePurpose::Title),
            ("alpha", UsagePurpose::Rebase),
            ("beta", UsagePurpose::Turn),
        ]
    );
    assert_eq!(folded.records[0].input_tokens, 400);
    assert_eq!(folded.records[0].cost_micro_usd, Some(30));
    assert_eq!(folded.records[2].input_tokens, 50);
    assert_eq!(folded.records[3].unpriced_invocation_count, 1);
    // The session turn's fill wins over the rebase assist's.
    assert_eq!(
        folded.context,
        Some(ContextNow {
            tokens: 600,
            window_tokens: Some(1_000),
        })
    );
    assert_eq!(unchanged, stored);
    assert_eq!(
        LiveUsageRun::Attempt("attempt".to_string()).purpose(),
        UsagePurpose::Turn
    );
}

#[test]
fn test_with_live_picks_the_same_context_whatever_order_runs_arrive_in() {
    // Arrange
    let first_run = LiveUsageRun::Attempt("attempt-a".to_string());
    let second_run = LiveUsageRun::Attempt("attempt-b".to_string());
    let first_usage = TurnUsage::single(detailed_usage("alpha", 100, None));
    let second_usage = TurnUsage::single(detailed_usage("beta", 300, None));
    let stored = SessionUsage::default();

    // Act
    let forward = stored.with_live([(&first_run, &first_usage), (&second_run, &second_usage)]);
    let backward = stored.with_live([(&second_run, &second_usage), (&first_run, &first_usage)]);

    // Assert
    assert_eq!(forward, backward);
    assert_eq!(forward.context.map(|context| context.tokens), Some(300));
}

#[test]
fn test_with_live_keeps_stored_context_when_the_turn_reported_none() {
    // Arrange
    let stored = SessionUsage {
        context: Some(ContextNow {
            tokens: 50,
            window_tokens: None,
        }),
        ..SessionUsage::default()
    };
    let live = TurnUsage::single(ModelUsage::unreported("model", 1));

    // Act
    let folded = stored.with_live([(&LiveUsageRun::SessionTurn, &live)]);

    // Assert
    assert_eq!(folded.context, stored.context);
    assert_eq!(folded.records.len(), 1);
}

#[test]
fn test_sort_records_orders_by_model_then_purpose() {
    // Arrange
    let purposes = [
        UsagePurpose::Other,
        UsagePurpose::Rebase,
        UsagePurpose::Review,
        UsagePurpose::CommitAssist,
        UsagePurpose::Title,
        UsagePurpose::Turn,
    ];
    let mut usage = SessionUsage {
        records: purposes
            .iter()
            .map(|purpose| UsageRecord::from_model_usage(*purpose, &ModelUsage::new("model")))
            .collect(),
        ..SessionUsage::default()
    };

    // Act
    usage.sort_records();

    // Assert
    let sorted = usage
        .records
        .iter()
        .map(|record| record.purpose)
        .collect::<Vec<_>>();
    assert_eq!(
        sorted,
        [
            UsagePurpose::Turn,
            UsagePurpose::Title,
            UsagePurpose::CommitAssist,
            UsagePurpose::Review,
            UsagePurpose::Rebase,
            UsagePurpose::Other,
        ]
    );
}

#[test]
fn test_parse_labels_round_trip_and_fall_back() {
    // Arrange
    let purposes = [
        UsagePurpose::Turn,
        UsagePurpose::Title,
        UsagePurpose::CommitAssist,
        UsagePurpose::Review,
        UsagePurpose::Rebase,
        UsagePurpose::Other,
    ];
    let bases = [
        CostBasis::ListPrice,
        CostBasis::HostPrice,
        CostBasis::AgentReported,
        CostBasis::Mixed,
    ];

    // Act
    let parsed_purposes = purposes.map(|purpose| parse_usage_purpose(purpose.as_str()));
    let parsed_bases = bases.map(|basis| parse_cost_basis(basis.as_str()));

    // Assert
    assert_eq!(parsed_purposes, purposes);
    assert_eq!(parsed_bases, bases);
    assert_eq!(parse_usage_purpose("future"), UsagePurpose::Other);
    assert_eq!(parse_cost_basis("future"), CostBasis::Mixed);
}
