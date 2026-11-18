//! Cost arithmetic in micro-USD, per-turn usage accumulation, and session
//! totals over known, unknown, and unpriced turns.

use std::time::Duration;

use serde_json::json;

use super::{Cost, StoredTurnUsage, TurnUsage, UsageTotals};
use crate::catalog::{Pricing, PricingSource};
use crate::lifecycle::ModelResponseType;
use crate::model::{CompletionMetadata, CompletionUsage};
use crate::run::Usage;
use crate::turn::{ModelRequestActivity, TurnOutcome, TurnReport};

/// Returns usage of `input` and `output` tokens, `cache_read` of them read
/// from the provider's cache.
fn usage(input: u64, cache_read: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        ..Usage::default()
    }
}

/// Returns a request whose provider reported `input` and `output` tokens.
fn request(input: u64, output: u64) -> ModelRequestActivity {
    let usage = CompletionUsage::new(None, None, Some(input), Some(output), None, None);

    ModelRequestActivity::new(
        Some(CompletionMetadata::new(
            "stop".into(),
            None,
            None,
            None,
            Some(usage),
        )),
        Duration::ZERO,
        ModelResponseType::Output,
    )
}

#[test]
fn cost_bills_uncached_input_cached_input_and_output_at_their_prices() {
    // Arrange
    let pricing = Pricing::new(3_000_000, 15_000_000).with_cache_read(300_000);
    let mut tokens = usage(1_000, 400, 300);
    tokens.reasoning = 100;

    // Act
    let cost = Cost::of(&tokens, pricing);

    // Assert
    // 600 uncached input at $3, 400 cached at $0.30, and 300 output at $15
    // per million tokens; reasoning is already part of the output.
    assert_eq!(cost.micro_usd, 1_800 + 120 + 4_500);
    assert_eq!(cost.pricing, PricingSource::Host);
}

#[test]
fn cost_without_cache_prices_bills_cached_input_at_the_input_price() {
    // Arrange
    let pricing = Pricing::built_in(1_000_000, 2_000_000);
    let mut written = usage(1_000, 400, 0);
    written.cache_write = Some(200);

    // Act
    let read = Cost::of(&usage(1_000, 400, 10), pricing);
    let write = Cost::of(&written, pricing.with_cache_write(5_000_000));

    // Assert
    assert_eq!(read.micro_usd, 1_000 + 20);
    assert_eq!(read.pricing, PricingSource::BuiltIn);
    assert_eq!(write.micro_usd, 400 + 400 + 1_000);
    assert_eq!(write.pricing, PricingSource::Host);
}

#[test]
fn cost_rounds_to_the_nearest_micro_usd_with_halves_up() {
    // Arrange
    let one_token = usage(1, 0, 0);

    // Act
    let micro_usd = [499_999, 500_000, 1_499_999, 1_500_000]
        .map(|price| Cost::of(&one_token, Pricing::new(price, 0)).micro_usd);

    // Assert
    assert_eq!(micro_usd, [0, 1, 1, 2]);
}

#[test]
fn cost_saturates_instead_of_overflowing() {
    // Arrange
    let most = usage(u64::MAX, 0, u64::MAX);
    let exact = usage(u64::MAX, 0, 0);

    // Act
    let saturated = Cost::of(&most, Pricing::new(u64::MAX, u64::MAX));
    let fitting = Cost::of(&exact, Pricing::new(1_000_000, 0));
    let summed = fitting.saturating_add(fitting);

    // Assert
    assert_eq!(saturated.micro_usd, u64::MAX);
    assert_eq!(fitting.micro_usd, u64::MAX);
    assert_eq!(summed.micro_usd, u64::MAX);
}

#[test]
fn cost_of_cache_reads_beyond_the_input_count_bills_no_uncached_input() {
    // Arrange
    let inconsistent = usage(10, 50, 0);

    // Act
    let cost = Cost::of(
        &inconsistent,
        Pricing::new(1_000_000, 0).with_cache_read(100_000),
    );

    // Assert
    assert_eq!(cost.micro_usd, 5);
}

#[test]
fn report_cost_sums_each_requests_own_cost_and_needs_every_price() {
    // Arrange
    let pricing = Pricing::new(500_000, 0);
    let summary_pricing = Pricing::new(2_000_000, 0);
    let priced_at = |pricing: Option<Pricing>| {
        let activity = request(1, 0);
        let usage = Usage::from_report(&TurnReport::new(
            Duration::ZERO,
            vec![activity.clone()],
            Vec::new(),
        ));

        activity.with_answerer(None, pricing.map(|pricing| Cost::of(&usage, pricing)))
    };
    let report =
        |requests: Vec<ModelRequestActivity>| TurnReport::new(Duration::ZERO, requests, Vec::new());
    let mixed = report(vec![
        priced_at(Some(pricing)),
        priced_at(Some(pricing)),
        priced_at(Some(summary_pricing)),
    ]);
    let partly_unpriced = report(vec![priced_at(Some(pricing)), priced_at(None)]);

    // Act
    let priced = Cost::of_report(&mixed, Some(pricing));
    let unpriced_run = Cost::of_report(&mixed, None);
    let unpriced_request = Cost::of_report(&partly_unpriced, Some(pricing));
    let nothing = Cost::of_report(&report(Vec::new()), Some(pricing));

    // Assert
    assert_eq!(priced.map(|cost| cost.micro_usd), Some(4));
    assert_eq!(unpriced_run, None);
    assert_eq!(unpriced_request, None);
    assert_eq!(nothing.map(|cost| cost.micro_usd), Some(0));
}

#[test]
fn cost_serializes_as_micro_usd_and_a_snake_case_source() -> Result<(), serde_json::Error> {
    // Arrange
    let cost = Cost::of(&usage(1_000, 0, 0), Pricing::built_in(1_000_000, 0));

    // Act
    let encoded = serde_json::to_value(cost)?;
    let decoded: Cost = serde_json::from_value(encoded.clone())?;

    // Assert
    assert_eq!(encoded, json!({"micro_usd": 1_000, "pricing": "built_in"}));
    assert_eq!(decoded, cost);

    Ok(())
}

#[test]
fn turn_usage_starts_at_zero_and_settles_from_a_stored_outcome() {
    // Arrange
    let pricing = Pricing::new(1_000_000, 2_000_000);
    let recorded = TurnUsage {
        cost: Some(Cost::of(&usage(5, 0, 0), pricing)),
        usage: usage(5, 0, 0),
    };
    let outcome = TurnOutcome::new(
        json!({}),
        TurnReport::new(
            Duration::ZERO,
            vec![request(100, 10), request(200, 20)],
            Vec::new(),
        ),
    );

    // Act
    let priced = TurnUsage::new(Some(pricing));
    let unpriced = TurnUsage::new(None);
    let kept = TurnUsage::settled(Some(recorded), Some(&outcome));
    let derived = TurnUsage::settled(None, Some(&outcome));
    let unknown = TurnUsage::settled(None, None);

    // Assert
    assert_eq!(priced.cost.map(|cost| cost.micro_usd), Some(0));
    assert_eq!(priced.usage, Usage::default());
    assert_eq!(unpriced, TurnUsage::default());
    assert_eq!(kept, Some(recorded));
    assert_eq!(
        derived,
        Some(TurnUsage {
            cost: None,
            usage: usage(300, 0, 30),
        }),
        "a turn stored before usage was recorded reads its outcome, unpriced"
    );
    assert_eq!(unknown, None);
}

#[test]
fn request_usage_sums_every_request_and_is_zero_without_one() {
    // Arrange
    let requests = [request(1, 2), request(3, 4)];

    // Act
    let summed = Usage::from_requests(&requests);
    let none = Usage::from_requests(&[]);

    // Assert
    assert_eq!(summed, usage(4, 0, 6));
    assert_eq!(none, Usage::default());
}

#[test]
fn cost_sums_keep_a_shared_source_and_mix_different_ones() {
    // Arrange
    let host = Cost::of(&usage(1_000, 0, 0), Pricing::new(1_000_000, 0));
    let built_in = Cost::of(&usage(1_000, 0, 0), Pricing::built_in(1_000_000, 0));

    // Act
    let same = host.saturating_add(host);
    let mixed = host.saturating_add(built_in);

    // Assert
    assert_eq!((same.micro_usd, same.pricing), (2_000, PricingSource::Host));
    assert_eq!(
        (mixed.micro_usd, mixed.pricing),
        (2_000, PricingSource::Mixed)
    );
}

#[test]
fn stored_usage_decodes_its_golden_record_and_round_trips_every_source()
-> Result<(), serde_json::Error> {
    // Arrange
    let golden = r#"{"cache_read":200,"cache_write":null,"cost":{"micro_usd":3960,"pricing":"host"},"input":1000,"output":100,"reasoning":40,"unreported_requests":0}"#;
    let mut tokens = usage(1_000, 200, 100);
    tokens.reasoning = 40;
    let expected = TurnUsage {
        cost: Some(Cost {
            micro_usd: 3_960,
            pricing: PricingSource::Host,
        }),
        usage: tokens,
    };
    let sources = [
        PricingSource::BuiltIn,
        PricingSource::Host,
        PricingSource::Mixed,
    ];
    let mut turns: Vec<TurnUsage> = sources
        .into_iter()
        .map(|pricing| TurnUsage {
            cost: Some(Cost {
                micro_usd: 1,
                pricing,
            }),
            usage: tokens,
        })
        .collect();
    turns.push(TurnUsage::default());

    // Act
    let decoded = TurnUsage::from(serde_json::from_str::<StoredTurnUsage>(golden)?);
    let encoded = serde_json::to_string(&StoredTurnUsage::from(&expected))?;
    let round_trips = turns
        .iter()
        .map(|turn| {
            let stored = serde_json::to_string(&StoredTurnUsage::from(turn))?;

            Ok(TurnUsage::from(serde_json::from_str::<StoredTurnUsage>(
                &stored,
            )?))
        })
        .collect::<Result<Vec<_>, serde_json::Error>>()?;

    // Assert
    assert_eq!(decoded, expected, "records written so far keep decoding");
    assert_eq!(encoded, golden, "the stored record is unchanged");
    assert_eq!(round_trips, turns);

    Ok(())
}

#[test]
fn usage_sum_keeps_cache_writes_only_when_both_are_known() {
    // Arrange
    let mut known = usage(10, 0, 0);
    known.cache_write = Some(4);

    // Act
    let both = known.saturating_add(known);
    let one = known.saturating_add(usage(10, 0, 0));

    // Assert
    assert_eq!(both.cache_write, Some(8));
    assert_eq!(one.cache_write, None);
}

#[test]
fn totals_count_unknown_and_unpriced_turns_and_sum_the_rest() {
    // Arrange
    let pricing = Pricing::new(1_000_000, 0);
    let mut cached = usage(100, 0, 5);
    cached.cache_write = Some(7);
    let priced = TurnUsage {
        cost: Some(Cost::of(&cached, pricing)),
        usage: cached,
    };
    let unpriced = TurnUsage {
        cost: None,
        usage: usage(20, 0, 1),
    };
    let listed = TurnUsage {
        cost: Some(Cost::of(&cached, Pricing::built_in(1_000_000, 0))),
        usage: cached,
    };
    let turns = [Some(priced), None, Some(unpriced), Some(priced)];

    // Act
    let totals = UsageTotals::from_turns(&turns);
    let only_unpriced = UsageTotals::from_turns(&[Some(TurnUsage {
        cost: None,
        usage: cached,
    })]);
    let mixed = UsageTotals::from_turns(&[Some(priced), Some(listed)]);

    // Assert
    assert_eq!(totals.turns, 4);
    assert_eq!(totals.unknown_turns, 1);
    assert_eq!(totals.unpriced_turns, 1);
    assert_eq!(
        totals.cost,
        Some(Cost {
            micro_usd: 200,
            pricing: PricingSource::Host,
        })
    );
    assert_eq!(totals.usage, usage(220, 0, 11));
    assert_eq!(only_unpriced.usage.cache_write, Some(7));
    assert_eq!(
        only_unpriced.cost, None,
        "no priced turn is no cost, not zero"
    );
    assert_eq!(
        mixed.cost.map(|cost| (cost.micro_usd, cost.pricing)),
        Some((200, PricingSource::Mixed))
    );
    assert_eq!(UsageTotals::from_turns(&[]), UsageTotals::default());
}
