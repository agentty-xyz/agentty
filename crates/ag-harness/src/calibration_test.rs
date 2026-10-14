use std::num::NonZeroU64;

use crate::calibration::{ContextCalibration, Envelope};
use crate::context::ContextBudget;
use crate::model::{CompletionMetadata, CompletionUsage};
use crate::turn::TurnError;

/// A budget of 1,100 with 100 reserved, so the request capacity is 1,000
/// and the slope needs 100 weight units of growth.
fn budget() -> ContextBudget {
    ContextBudget::new(NonZeroU64::new(1_100).expect("nonzero budget"))
        .with_reserved_output(100)
        .expect("reserve within budget")
}

fn completion(input_tokens: Option<u64>) -> CompletionMetadata {
    CompletionMetadata::new(
        "stop".to_string(),
        None,
        None,
        None,
        Some(CompletionUsage::new(
            None,
            None,
            input_tokens,
            Some(1),
            None,
            None,
        )),
    )
}

/// Returns a calibration that observed each `(estimated weight, input
/// tokens)` report in order.
fn calibrated(reports: &[(u64, u64)]) -> ContextCalibration {
    let calibration = ContextCalibration::default();
    for (weight, tokens) in reports {
        calibration.observe(budget(), *weight, Some(&completion(Some(*tokens))));
    }

    calibration
}

/// Returns the effective request capacity left after reserved output.
fn capacity(calibration: &ContextCalibration) -> u64 {
    let (effective, _) = calibration.effective_budget(budget());
    assert_eq!(effective.reserved_output_weight(), 100);

    effective.max_request_weight().get() - 100
}

/// Returns the calibrated tokens of a request of `weight`.
fn tokens(calibration: &ContextCalibration, weight: u64) -> u64 {
    calibration
        .effective_budget(budget())
        .1
        .envelope()
        .expect("calibrated")
        .tokens(weight)
}

#[test]
fn uncalibrated_budget_keeps_ninety_percent_of_the_capacity() {
    // Arrange
    let calibration = ContextCalibration::default();

    // Act
    let (_, envelope) = calibration.effective_budget(budget());

    // Assert
    assert_eq!(envelope.envelope(), None);
    assert_eq!(capacity(&calibration), 900);
}

#[test]
fn the_first_report_calibrates_its_ratio() {
    // Arrange
    let calibration = calibrated(&[(400, 500)]);

    // Act
    let capacity = capacity(&calibration);

    // Assert
    assert_eq!(capacity, 760);
    assert_eq!(tokens(&calibration, 760), 950);
    assert_eq!(tokens(&calibration, 200), 250);
    assert_eq!(tokens(&calibration, 0), 0);
}

#[test]
fn growth_separates_the_fixed_cost_from_the_slope() {
    // Arrange
    let calibration = calibrated(&[(100, 600)]);
    let ratio_only = capacity(&calibration);

    // Act
    calibration.observe(budget(), 500, Some(&completion(Some(1_080))));

    // Assert
    assert_eq!(ratio_only, 216);
    assert_eq!(tokens(&calibration, 500), 1_080);
    assert_eq!(capacity(&calibration), 391);
    assert_eq!(tokens(&calibration, 391), 950);
}

#[test]
fn small_growth_lifts_the_envelope_without_measuring_the_slope() {
    // Arrange
    let calibration = calibrated(&[(400, 500)]);

    // Act
    calibration.observe(budget(), 450, Some(&completion(Some(600))));

    // Assert
    assert_eq!(tokens(&calibration, 450), 600);
    assert_eq!(tokens(&calibration, 400), 537);
}

#[test]
fn measured_slopes_only_rise() {
    // Arrange
    let calibration = calibrated(&[(100, 600), (500, 1_080)]);

    // Act
    calibration.observe(budget(), 900, Some(&completion(Some(1_400))));
    let kept = tokens(&calibration, 900);
    calibration.observe(budget(), 700, Some(&completion(Some(1_400))));

    // Assert
    assert_eq!(kept, 1_560);
    assert_eq!(tokens(&calibration, 700), 1_401);
}

#[test]
fn reports_above_the_envelope_lift_it() {
    // Arrange
    let calibration = calibrated(&[(100, 600), (500, 1_080)]);

    // Act
    calibration.observe(budget(), 150, Some(&completion(Some(700))));

    // Assert
    assert_eq!(tokens(&calibration, 150), 700);
    assert_eq!(tokens(&calibration, 500), 1_120);
}

#[test]
fn over_estimates_never_loosen_the_declared_budget() {
    // Arrange
    let calibration = calibrated(&[(800, 200)]);

    // Act
    let (effective, _) = calibration.effective_budget(budget());

    // Assert
    assert_eq!(effective, budget());
}

#[test]
fn reports_beyond_the_capacity_leave_one_unit() {
    // Arrange
    let calibration = calibrated(&[(200, 100_000)]);

    // Act
    let capacity = capacity(&calibration);

    // Assert
    assert_eq!(capacity, 1);
    assert_eq!(tokens(&calibration, 201), 100_003);
}

#[test]
fn responses_without_reported_input_tokens_leave_the_estimate_uncalibrated() {
    // Arrange
    let calibration = ContextCalibration::default();
    let without_usage = CompletionMetadata::new("stop".to_string(), None, None, None, None);

    // Act
    calibration.observe(budget(), 400, None);
    calibration.observe(budget(), 400, Some(&without_usage));
    calibration.observe(budget(), 400, Some(&completion(None)));
    calibration.observe(budget(), 0, Some(&completion(Some(250))));

    // Assert
    assert_eq!(calibration.effective_budget(budget()).1.envelope(), None);
}

#[test]
fn clones_share_one_calibration() {
    // Arrange
    let calibration = ContextCalibration::default();
    let shared = calibration.clone();

    // Act
    shared.observe(budget(), 400, Some(&completion(Some(800))));

    // Assert
    assert_eq!(tokens(&calibration, 400), 800);
}

#[test]
fn uncalibrated_budgets_keep_at_least_one_unit_of_capacity() {
    // Arrange
    let budget = ContextBudget::new(NonZeroU64::new(2).expect("nonzero budget"))
        .with_reserved_output(1)
        .expect("reserve within budget");

    // Act
    let (effective, _) = ContextCalibration::default().effective_budget(budget);

    // Assert
    assert_eq!(effective.max_request_weight().get(), 2);
}

#[test]
fn budget_failures_are_restated_in_declared_units() {
    // Arrange
    let (_, uncalibrated) = ContextCalibration::default().effective_budget(budget());
    let (_, calibrated_envelope) =
        calibrated(&[(100, 600), (500, 1_080)]).effective_budget(budget());
    let (_, over_estimated) = calibrated(&[(800, 200)]).effective_budget(budget());
    let exceeded = |required| TurnError::ContextBudgetExceeded {
        budget: 0,
        required,
    };

    // Act
    let failures = [
        ContextCalibration::declared_failure(budget(), uncalibrated, exceeded(701)),
        ContextCalibration::declared_failure(budget(), calibrated_envelope, exceeded(701)),
        ContextCalibration::declared_failure(budget(), over_estimated, exceeded(1_101)),
    ];

    // Assert
    let restated: Vec<(u64, u64)> = failures
        .iter()
        .map(|failure| match failure {
            TurnError::ContextBudgetExceeded { budget, required } => (*budget, *required),
            _ => unreachable!("budget failures stay budget failures"),
        })
        .collect();
    assert_eq!(restated, [(1_100, 768), (1_100, 1_366), (1_100, 1_101)]);
}

#[test]
fn other_failures_pass_through_unchanged() {
    // Arrange
    let (_, envelope) = ContextCalibration::default().effective_budget(budget());

    // Act
    let declared = ContextCalibration::declared_failure(budget(), envelope, TurnError::Cancelled);

    // Assert
    assert!(matches!(declared, TurnError::Cancelled));
}

/// Envelopes across the clamped slope range, measured and lifted, with
/// bases that put capacities on both sides of the base request.
fn sample_envelopes() -> Vec<Envelope> {
    let mut envelopes = Vec::new();
    for slope_permille in [1_000, 1_001, 1_333, 1_500, 2_333, 2_999, 3_000] {
        for (base_weight, base_tokens, lift) in [(0, 0, 0), (40, 300, 0), (250, 420, 37)] {
            envelopes.push(Envelope {
                base_tokens,
                base_weight,
                lift,
                measured: lift > 0,
                slope_permille,
            });
        }
    }

    envelopes
}

#[test]
fn weight_within_is_the_largest_weight_whose_tokens_fit() {
    // Arrange
    let envelopes = sample_envelopes();

    for envelope in envelopes {
        for capacity in 0..1_200 {
            if envelope.tokens(0) > capacity {
                continue;
            }

            // Act
            let weight = envelope.weight_within(capacity);

            // Assert
            assert!(
                envelope.tokens(weight) <= capacity,
                "{envelope:?} weight {weight} exceeds capacity {capacity}"
            );
            assert!(
                envelope.tokens(weight + 1) > capacity,
                "{envelope:?} weight {} also fits capacity {capacity}",
                weight + 1
            );
        }
    }
}

#[test]
fn restated_failures_exceed_the_declared_budget_exactly_when_the_effective_one_is_exceeded() {
    // Arrange
    let calibrations = [
        ContextCalibration::default(),
        calibrated(&[(100, 600), (500, 1_080)]),
        calibrated(&[(400, 500)]),
        calibrated(&[(800, 200)]),
    ];

    for calibration in calibrations {
        let (effective, envelope) = calibration.effective_budget(budget());
        let effective_capacity = effective.max_request_weight().get() - 100;
        for weight in effective_capacity.saturating_sub(50)..effective_capacity + 50 {
            // Act
            let restated = ContextCalibration::declared_failure(
                budget(),
                envelope,
                TurnError::ContextBudgetExceeded {
                    budget: effective.max_request_weight().get(),
                    required: weight + 100,
                },
            );

            // Assert
            let TurnError::ContextBudgetExceeded { budget, required } = restated else {
                unreachable!("budget failures stay budget failures");
            };
            assert_eq!(
                required > budget,
                weight > effective_capacity,
                "{envelope:?} weight {weight} restated as {required} of {budget}"
            );
        }
    }
}

#[test]
fn overflows_cap_capacity_below_the_rejected_weight() {
    // Arrange
    let uncalibrated = ContextCalibration::default();
    let calibrated = calibrated(&[(400, 500)]);

    // Act
    uncalibrated.observe_overflow(800);
    calibrated.observe_overflow(300);

    // Assert
    assert_eq!(capacity(&uncalibrated), 600);
    assert_eq!(capacity(&calibrated), 225);
}

#[test]
fn overflow_ceilings_only_fall() {
    // Arrange
    let calibration = ContextCalibration::default();
    calibration.observe_overflow(400);

    // Act
    calibration.observe_overflow(800);
    let after_larger = capacity(&calibration);
    calibration.observe_overflow(200);

    // Assert
    assert_eq!(after_larger, 300);
    assert_eq!(capacity(&calibration), 150);
}

#[test]
fn restated_failures_exceed_the_declared_budget_exactly_when_an_overflow_ceiling_rejects_them() {
    // Arrange
    let calibrations = [ContextCalibration::default(), calibrated(&[(400, 500)])];
    for (calibration, rejected) in calibrations.iter().zip([600, 260]) {
        calibration.observe_overflow(rejected);
    }
    let floor = ContextCalibration::default();
    floor.observe_overflow(1);

    for calibration in calibrations.iter().chain([&floor]) {
        let (effective, state) = calibration.effective_budget(budget());
        let effective_capacity = effective.max_request_weight().get() - 100;
        for weight in effective_capacity.saturating_sub(50)..effective_capacity + 50 {
            // Act
            let restated = ContextCalibration::declared_failure(
                budget(),
                state,
                TurnError::ContextBudgetExceeded {
                    budget: effective.max_request_weight().get(),
                    required: weight + 100,
                },
            );

            // Assert
            let TurnError::ContextBudgetExceeded { budget, required } = restated else {
                unreachable!("budget failures stay budget failures");
            };
            assert_eq!(
                required > budget,
                weight > effective_capacity,
                "{state:?} weight {weight} restated as {required} of {budget}"
            );
        }
    }
}
