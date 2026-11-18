//! Budget ledger unit tests: `SharedBudget` admission against its caps,
//! waiting on open reservations, commits that replace estimates with actual
//! spend, releases of dropped reservations, and the `Unlimited` default.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::task::{JoinHandle, yield_now};

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

/// Starts reserving `estimate` on a clone of `budget` and lets the
/// reservation run until it waits or finishes.
async fn spawn_reserve(
    budget: &SharedBudget,
    estimate: Estimate,
) -> JoinHandle<Result<Reservation, LimitKind>> {
    let budget = budget.clone();
    let task = tokio::spawn(async move { budget.reserve(estimate).await });
    yield_now().await;

    task
}

#[tokio::test]
async fn shared_budget_refuses_an_estimate_that_passes_its_cost_cap_beside_spend() {
    // Arrange
    let budget = SharedBudget::new().with_max_micro_usd(250);
    let estimate = Estimate::new(10, 5).with_cost(cost(100));
    for _ in 0..2 {
        let reservation = budget.reserve(estimate).await.expect("estimate fits");
        budget.commit(reservation, &usage(10, 5), Some(cost(100)));
    }

    // Act
    let refused = budget.reserve(estimate).await;

    // Assert
    assert_eq!(refused.err(), Some(LimitKind::Cost));
    let balance = budget.balance();
    assert_eq!(
        (balance.spent_micro_usd, balance.reserved_micro_usd),
        (200, 0)
    );
}

#[tokio::test]
async fn shared_budget_refuses_an_estimate_that_passes_its_token_cap_alone() {
    // Arrange
    let budget = SharedBudget::new()
        .with_max_micro_usd(1_000)
        .with_max_tokens(20);

    // Act
    let admitted = budget.reserve(Estimate::new(10, 5)).await;
    let refused = budget.reserve(Estimate::new(15, 6)).await;

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
    let budget = SharedBudget::new()
        .with_max_micro_usd(50)
        .with_max_tokens(5);

    // Act
    let refused = budget
        .reserve(Estimate::new(10, 5).with_cost(cost(100)))
        .await;

    // Assert
    assert_eq!(refused.err(), Some(LimitKind::Cost));
}

#[tokio::test(start_paused = true)]
async fn a_reservation_blocked_only_by_open_reservations_waits_and_fits_once_they_settle_below_estimate()
 {
    // Arrange
    let budget = SharedBudget::new().with_max_micro_usd(250);
    let estimate = Estimate::new(10, 5).with_cost(cost(100));
    let first = budget.reserve(estimate).await.expect("estimate fits");
    let second = budget.reserve(estimate).await.expect("estimate fits");
    let waiting = spawn_reserve(&budget, estimate).await;
    let blocked = !waiting.is_finished();

    // Act
    budget.commit(first, &usage(1, 1), Some(cost(10)));
    budget.commit(second, &usage(1, 1), Some(cost(10)));
    let admitted = waiting.await.expect("reserve task");

    // Assert
    assert!(blocked, "open reservations keep the third request waiting");
    assert!(admitted.is_ok(), "the settled requests left room for it");
    let balance = budget.balance();
    assert_eq!(
        (balance.spent_micro_usd, balance.reserved_micro_usd),
        (20, 100)
    );
}

#[tokio::test(start_paused = true)]
async fn a_waiting_reservation_is_refused_once_committed_spend_leaves_no_room() {
    // Arrange
    let budget = SharedBudget::new().with_max_tokens(20);
    let open = budget.reserve(Estimate::new(10, 5)).await.expect("fits");
    let waiting = spawn_reserve(&budget, Estimate::new(10, 0)).await;
    let blocked = !waiting.is_finished();

    // Act
    budget.commit(open, &usage(12, 5), None);
    let refused = waiting.await.expect("reserve task");

    // Assert
    assert!(blocked);
    assert_eq!(refused.err(), Some(LimitKind::Tokens));
}

#[tokio::test]
async fn commit_replaces_the_estimate_with_actual_spend() {
    // Arrange
    let budget = SharedBudget::new().with_max_micro_usd(1_000);
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
    let budget = SharedBudget::new();
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
    let budget = SharedBudget::new().with_max_micro_usd(0);

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

#[tokio::test(start_paused = true)]
async fn dropping_a_reservation_releases_it_and_wakes_waiting_reservations() {
    // Arrange
    let budget = SharedBudget::new().with_max_tokens(15);
    let reservation = budget
        .reserve(Estimate::new(10, 5))
        .await
        .expect("estimate fits");
    let waiting = spawn_reserve(&budget, Estimate::new(10, 5)).await;
    let blocked = !waiting.is_finished();

    // Act
    drop(reservation);
    let admitted = waiting.await.expect("reserve task");

    // Assert
    assert!(blocked);
    assert!(admitted.is_ok());
    assert_eq!(budget.balance().spent_tokens, 0);
}

#[tokio::test]
async fn clones_share_one_account() {
    // Arrange
    let budget = SharedBudget::new().with_max_tokens(100);
    let clone = budget.clone();

    // Act
    let reservation = clone.reserve(Estimate::new(30, 10)).await.expect("fits");
    clone.commit(reservation, &usage(30, 10), None);

    // Assert
    assert_eq!(budget.balance().spent_tokens, 40);
}

#[tokio::test]
async fn committed_spend_exceeds_the_cap_only_by_overshoot_beyond_estimates() {
    // Arrange
    let budget = SharedBudget::new().with_max_tokens(100);
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
async fn unlimited_admits_saturating_estimates_while_others_are_open() {
    // Arrange
    let ledger = Unlimited;
    let huge = Estimate::new(u64::MAX, u64::MAX).with_cost(cost(u64::MAX));

    // Act
    let first = ledger.reserve(huge).await.expect("unlimited admits");
    let second = ledger.reserve(huge).await.expect("unlimited admits again");
    ledger.commit(first, &usage(1, 1), None);

    // Assert
    assert_eq!(second.estimate().tokens(), u64::MAX);
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
