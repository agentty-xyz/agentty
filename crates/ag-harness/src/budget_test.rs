//! Budget ledger unit tests: `SharedBudget` admission against its caps,
//! commits that replace estimates with actual spend, releases of dropped
//! reservations, and the `Unlimited` default.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::budget::{Balance, BudgetLedger, Estimate, Reservation, SharedBudget, Unlimited};
use crate::catalog::PricingSource;
use crate::limit::LimitKind;
use crate::run::Usage;
use crate::usage::Cost;

/// Returns a cost of `micro_usd` from host prices.
fn cost(micro_usd: u64) -> Cost {
    Cost {
        micro_usd,
        pricing: PricingSource::Host,
    }
}

/// Returns reported usage of `input` and `output` tokens.
fn usage(input: u64, output: u64) -> Usage {
    Usage {
        input,
        output,
        ..Usage::default()
    }
}

#[tokio::test]
async fn shared_budget_refuses_an_estimate_that_would_pass_its_cost_cap() {
    // Arrange
    let budget = SharedBudget::default().with_max_micro_usd(250);
    let estimate = Estimate::new(10, 5).with_cost(cost(100));

    // Act
    let first = budget.reserve(estimate).await;
    let second = budget.reserve(estimate).await;
    let third = budget.reserve(estimate).await;

    // Assert
    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(third.err(), Some(LimitKind::Cost));
    assert_eq!(budget.balance().reserved_micro_usd, 200);
    assert_eq!(budget.balance().reserved_tokens, 30);
}

#[tokio::test]
async fn shared_budget_refuses_an_estimate_that_would_pass_its_token_cap() {
    // Arrange
    let budget = SharedBudget::default()
        .with_max_micro_usd(1_000)
        .with_max_tokens(20);

    // Act
    let admitted = budget.reserve(Estimate::new(10, 5)).await;
    let refused = budget.reserve(Estimate::new(4, 2)).await;

    // Assert
    assert_eq!(
        admitted.map(|reservation| reservation.estimate().tokens()),
        Ok(15)
    );
    assert_eq!(refused.err(), Some(LimitKind::Tokens));
}

#[tokio::test]
async fn shared_budget_checks_cost_before_tokens() {
    // Arrange
    let budget = SharedBudget::default()
        .with_max_micro_usd(50)
        .with_max_tokens(5);

    // Act
    let refused = budget
        .reserve(Estimate::new(10, 5).with_cost(cost(100)))
        .await;

    // Assert
    assert_eq!(refused.err(), Some(LimitKind::Cost));
}

#[tokio::test]
async fn commit_replaces_the_estimate_with_actual_spend() {
    // Arrange
    let budget = SharedBudget::default().with_max_micro_usd(1_000);
    let reservation = budget
        .reserve(Estimate::new(100, 50).with_cost(cost(300)))
        .await
        .expect("estimate fits");

    // Act
    budget.commit(reservation, &usage(80, 20), Some(cost(120)));

    // Assert
    assert_eq!(
        budget.balance(),
        Balance {
            max_micro_usd: Some(1_000),
            max_tokens: None,
            reserved_micro_usd: 0,
            reserved_tokens: 0,
            spent_micro_usd: 120,
            spent_tokens: 100,
        }
    );
}

#[tokio::test]
async fn commit_without_reported_usage_charges_the_estimate() {
    // Arrange
    let budget = SharedBudget::default();
    let reservation = budget
        .reserve(Estimate::new(100, 50).with_cost(cost(300)))
        .await
        .expect("uncapped budget admits");
    let unreported = Usage {
        unreported_requests: 1,
        ..Usage::default()
    };

    // Act
    budget.commit(reservation, &unreported, Some(cost(0)));

    // Assert
    let balance = budget.balance();
    assert_eq!((balance.spent_micro_usd, balance.spent_tokens), (300, 150));
}

#[tokio::test]
async fn unpriced_requests_cost_nothing_against_the_cost_cap() {
    // Arrange
    let budget = SharedBudget::default().with_max_micro_usd(0);

    // Act
    let reservation = budget
        .reserve(Estimate::new(100, 50))
        .await
        .expect("an unpriced estimate costs nothing");
    budget.commit(reservation, &usage(100, 50), None);

    // Assert
    let balance = budget.balance();
    assert_eq!((balance.spent_micro_usd, balance.spent_tokens), (0, 150));
}

#[tokio::test]
async fn dropping_a_reservation_releases_it() {
    // Arrange
    let budget = SharedBudget::default().with_max_tokens(15);
    let reservation = budget
        .reserve(Estimate::new(10, 5))
        .await
        .expect("estimate fits");
    let refused = budget.reserve(Estimate::new(1, 0)).await;

    // Act
    drop(reservation);
    let admitted = budget.reserve(Estimate::new(10, 5)).await;

    // Assert
    assert_eq!(refused.err(), Some(LimitKind::Tokens));
    assert!(admitted.is_ok());
}

#[tokio::test]
async fn committed_spend_exceeds_the_cap_only_by_overshoot_beyond_estimates() {
    // Arrange
    let budget = SharedBudget::default().with_max_tokens(100);
    let mut overshoot = 0;

    // Act
    loop {
        let Ok(reservation) = budget.reserve(Estimate::new(30, 10)).await else {
            break;
        };
        let actual = usage(45, 10);
        overshoot += actual.input + actual.output - reservation.estimate().tokens();
        budget.commit(reservation, &actual, None);
    }

    // Assert
    let balance = budget.balance();
    assert_eq!(balance.spent_tokens, 110);
    assert!(balance.spent_tokens > 100);
    assert!(balance.spent_tokens <= 100 + overshoot);
}

#[tokio::test]
async fn unlimited_admits_everything_and_records_nothing() {
    // Arrange
    let ledger = Unlimited;

    // Act
    let reservation = ledger
        .reserve(Estimate::new(u64::MAX, u64::MAX).with_cost(cost(u64::MAX)))
        .await
        .expect("unlimited admits");
    let estimate = reservation.estimate();
    ledger.commit(reservation, &usage(1, 1), None);

    // Assert
    assert_eq!(estimate.tokens(), u64::MAX);
}

#[test]
fn a_reservation_runs_its_release_only_when_dropped_unsettled() {
    // Arrange
    let released = Arc::new(AtomicU64::new(0));
    let reservation = |released: &Arc<AtomicU64>| {
        let released = Arc::clone(released);

        Reservation::new(Estimate::new(3, 4), move |estimate| {
            released.fetch_add(estimate.tokens(), Ordering::SeqCst);
        })
    };

    // Act
    let settled = reservation(&released).settle();
    drop(reservation(&released));

    // Assert
    assert_eq!(settled, Estimate::new(3, 4));
    assert_eq!(released.load(Ordering::SeqCst), 7);
    assert_eq!(
        format!("{:?}", reservation(&released)),
        "Reservation { estimate: Estimate { cost: None, input_tokens: 3, output_tokens: 4 }, .. }"
    );
}
