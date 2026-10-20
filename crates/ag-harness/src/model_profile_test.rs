use std::num::NonZeroU64;

use crate::context::{ContextBudget, ContextBudgetError, HeuristicContextEstimator};
use crate::model_profile::ModelProfile;

#[test]
fn profile_reserves_declared_tokens_from_the_window() {
    // Arrange
    let unreserved = ModelProfile::new(128_000);

    // Act
    let reserved = unreserved.with_reserved_tokens(8_000);

    // Assert
    assert_eq!(unreserved.reserved_tokens(), 0);
    assert_eq!(reserved.context_window_tokens(), 128_000);
    assert_eq!(reserved.reserved_tokens(), 8_000);
    assert_eq!(reserved.input_token_budget(), 120_000);
}

#[test]
fn estimate_margin_fits_a_calibrated_request_that_under_counts_by_the_declared_percent() {
    // Arrange
    let profile = ModelProfile::new(400_000);

    // Act
    let margined = profile.with_estimate_margin(32_000, 15);

    // Assert
    let input_tokens = margined.input_token_budget();
    let output_free_permyriad = (400_000 - 32_000) * 10_000;
    assert_eq!(margined.context_window_tokens(), 400_000);
    assert_eq!(margined.reserved_tokens(), 63_158);
    assert_eq!(input_tokens, 336_842);
    assert!(input_tokens * 95 * 115 <= output_free_permyriad);
    assert!((input_tokens + 1) * 95 * 115 > output_free_permyriad);
}

#[test]
fn estimate_margin_reserves_only_output_for_an_under_count_the_unfilled_share_absorbs() {
    // Arrange
    let profile = ModelProfile::new(400_000);

    // Act
    let exact = profile.with_estimate_margin(32_000, 0);
    let absorbed = profile.with_estimate_margin(32_000, 5);

    // Assert
    assert_eq!(exact.reserved_tokens(), 32_000);
    assert_eq!(absorbed.reserved_tokens(), 32_000);
    assert_eq!(absorbed.input_token_budget(), 368_000);
}

#[test]
fn estimate_margin_saturates_instead_of_overflowing() {
    // Arrange
    let profile = ModelProfile::new(400_000);

    // Act
    let output_saturated = profile.with_estimate_margin(u64::MAX, 15);
    let under_count_saturated = profile.with_estimate_margin(32_000, u64::MAX);
    let window_saturated = ModelProfile::new(u64::MAX).with_estimate_margin(0, 0);

    // Assert
    assert_eq!(output_saturated.reserved_tokens(), u64::MAX);
    assert_eq!(output_saturated.input_token_budget(), 0);
    assert_eq!(under_count_saturated.reserved_tokens(), 400_000);
    assert_eq!(under_count_saturated.input_token_budget(), 0);
    assert_eq!(window_saturated.reserved_tokens(), 0);
}

#[test]
fn context_budget_holds_the_reservation_back_as_reserved_output() {
    // Arrange
    let profile = ModelProfile::new(262_144).with_estimate_margin(32_768, 20);

    // Act
    let budget = profile.context_budget();

    // Assert
    let expected = ContextBudget::new(NonZeroU64::new(262_144).expect("nonzero window"))
        .with_reserved_output(60_937)
        .expect("valid reservation");
    assert_eq!(budget, Ok(expected));
}

#[test]
fn context_budget_rejects_a_profile_without_input_capacity() {
    // Arrange
    let covered = ModelProfile::new(100).with_reserved_tokens(100);
    let empty = ModelProfile::new(0);

    // Act
    let covered_budget = covered.context_budget();
    let empty_budget = empty.context_budget();

    // Assert
    assert_eq!(covered.input_token_budget(), 0);
    assert_eq!(
        covered_budget,
        Err(ContextBudgetError::ReservedOutputExceedsBudget {
            max_request_weight: 100,
            reserved_output_weight: 100,
        })
    );
    assert_eq!(
        empty_budget,
        Err(ContextBudgetError::ReservedOutputExceedsBudget {
            max_request_weight: 0,
            reserved_output_weight: 0,
        })
    );
}

#[test]
fn profile_carries_its_declared_estimator() {
    // Arrange
    let estimator = HeuristicContextEstimator::DEFAULT.with_request_tokens(250);

    // Act
    let declared = ModelProfile::new(128_000).with_estimator(estimator);

    // Assert
    assert_eq!(
        ModelProfile::new(128_000).estimator(),
        HeuristicContextEstimator::DEFAULT
    );
    assert_eq!(declared.estimator(), estimator);
    assert_eq!(
        declared.context_budget(),
        ModelProfile::new(128_000).context_budget()
    );
}
