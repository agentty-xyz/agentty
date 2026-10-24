use std::sync::Barrier;

use crate::ProviderCallBudget;

#[test]
fn workflow_ceilings_preserve_local_limits_and_refund_unused_reservations() {
    for (local_limit, outer_limit, consume) in
        [(1, 3, true), (3, 1, true), (1, 1, false), (1, 0, false)]
    {
        // Arrange
        let local = ProviderCallBudget::new(local_limit);
        let outer = ProviderCallBudget::new(outer_limit);
        let constrained = local.clone().with_ceiling(outer.clone());

        // Act
        let reservation = constrained.reserve();

        // Assert
        if outer_limit == 0 {
            assert!(reservation.is_err());
            assert!(constrained.consume().is_err());
            assert_eq!(local.remaining(), local_limit);
            continue;
        }
        let reservation = reservation.expect("reserve both limits");
        assert_eq!(reservation.remaining(), 1);
        if consume {
            reservation.consume().expect("charge both counters");
        }
        drop(reservation);
        assert_eq!(local.remaining(), local_limit - usize::from(consume));
        assert_eq!(outer.remaining(), outer_limit - usize::from(consume));
        assert_eq!(constrained.remaining(), usize::from(!consume));
        if consume {
            assert!(constrained.consume().is_err());
        }
    }
}

#[test]
fn concurrent_ceiling_consumers_charge_only_successful_attempts() {
    for (local_limit, opposite_order, reserved) in
        [(2, false, false), (1, true, false), (2, false, true)]
    {
        // Arrange
        let rounds: Vec<_> = (0..128)
            .map(|_| {
                let local = ProviderCallBudget::new(local_limit);
                let outer = ProviderCallBudget::new(1);
                let forward = local.clone().with_ceiling(outer.clone());
                let forward = if reserved {
                    forward.reserve().expect("initial credit")
                } else {
                    forward
                };
                let backward = if opposite_order {
                    outer.clone().with_ceiling(local.clone())
                } else {
                    forward.clone()
                };

                (local, outer, forward, backward)
            })
            .collect();
        let start = Barrier::new(8);

        // Act
        let outcomes = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|index| {
                    let rounds = &rounds;
                    let start = &start;
                    scope.spawn(move || {
                        rounds
                            .iter()
                            .map(|(_, _, forward, backward)| {
                                start.wait();

                                usize::from(
                                    if index % 2 == 0 { forward } else { backward }
                                        .consume()
                                        .is_ok(),
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();

            workers
                .into_iter()
                .map(|worker| worker.join().expect("consumer"))
                .collect::<Vec<_>>()
        });

        // Assert
        for (index, (local, outer, forward, backward)) in rounds.into_iter().enumerate() {
            let admitted: usize = outcomes.iter().map(|outcome| outcome[index]).sum();
            drop((forward, backward));
            assert!(admitted <= 1);
            assert_eq!(
                local.remaining(),
                local_limit - admitted,
                "rejected attempts refund local credit"
            );
            assert_eq!(
                outer.remaining(),
                1 - admitted,
                "overlapping chains cannot lose shared credit"
            );
        }
    }
}

#[test]
fn repeated_and_nested_ceilings_never_relax_a_limit() {
    // Arrange
    let budget = ProviderCallBudget::new(2);
    let outer = ProviderCallBudget::new(3);
    let parent = ProviderCallBudget::new(1);
    let constrained = budget
        .clone()
        .with_ceiling(budget.clone())
        .with_ceiling(outer.clone())
        .with_ceiling(parent.clone());

    // Act
    constrained.consume().expect("charge every distinct limit");

    // Assert
    assert_eq!(budget.remaining(), 1);
    assert_eq!(outer.remaining(), 2);
    assert_eq!(parent.remaining(), 0);
    assert!(constrained.consume().is_err());
}

#[test]
fn same_counter_with_a_new_ceiling_keeps_the_stricter_limit() {
    // Arrange
    let budget = ProviderCallBudget::new(2);
    let outer = ProviderCallBudget::new(1);
    let constrained = budget
        .clone()
        .with_ceiling(budget.clone().with_ceiling(outer.clone()));

    // Act
    constrained.consume().expect("one shared call");

    // Assert
    assert_eq!(budget.remaining(), 1);
    assert_eq!(outer.remaining(), 0);
    assert!(constrained.consume().is_err());
}

#[test]
fn overlapping_ceiling_chains_charge_each_counter_once_and_refund_reservations() {
    for overlap in 0..3 {
        for operation in ["consume", "reserve-consume", "reserve-drop"] {
            // Arrange
            let first = ProviderCallBudget::new(1);
            let second = ProviderCallBudget::new(1);
            let third = ProviderCallBudget::new(1);
            let constrained = match overlap {
                0 => first
                    .clone()
                    .with_ceiling(second.clone().with_ceiling(first.clone())),
                1 => first
                    .clone()
                    .with_ceiling(second.clone())
                    .with_ceiling(second.clone().with_ceiling(first.clone())),
                _ => first.clone().with_ceiling(
                    second.clone().with_ceiling(
                        third
                            .clone()
                            .with_ceiling(first.clone().with_ceiling(second.clone())),
                    ),
                ),
            };

            // Act
            if operation == "consume" {
                constrained.consume().expect("charge distinct counters");
            } else {
                let reserved = constrained.reserve().expect("reserve distinct counters");
                let clone = reserved.clone();
                drop(reserved);
                assert_eq!(first.remaining(), 0);
                assert_eq!(second.remaining(), 0);
                assert_eq!(clone.remaining(), 1);
                if operation == "reserve-consume" {
                    clone.consume().expect("redeem each reserved call once");
                }
                drop(clone);
            }

            // Assert
            let remaining = usize::from(operation == "reserve-drop");
            assert_eq!(first.remaining(), remaining);
            assert_eq!(second.remaining(), remaining);
            assert_eq!(third.remaining(), if overlap == 2 { remaining } else { 1 });
            assert_eq!(constrained.remaining(), remaining);
        }
    }
}

#[test]
fn duplicate_counters_retain_available_reserved_credit() {
    for reserved_position in ["head", "tail", "both"] {
        for consume in [false, true] {
            // Arrange
            let budget = ProviderCallBudget::new(1);
            let outer = ProviderCallBudget::new(1);
            let reserved = budget.reserve().expect("reserve shared credit");
            let head = if reserved_position == "tail" {
                budget.clone()
            } else {
                reserved.clone()
            };
            let tail = if reserved_position == "head" {
                budget.clone()
            } else {
                reserved.clone()
            };

            // Act
            let constrained = head.with_ceiling(outer.clone().with_ceiling(tail));
            let child = constrained
                .reserve()
                .expect("keep reserved credit on overlap");
            if consume {
                child.consume().expect("redeem retained credit");
            }
            drop(child);

            // Assert
            assert_eq!(constrained.remaining(), usize::from(!consume));
            assert_eq!(budget.remaining(), 0);
            assert_eq!(outer.remaining(), usize::from(!consume));
            drop(constrained);
            drop(reserved);
            assert_eq!(budget.remaining(), usize::from(!consume));
        }
    }
}

#[test]
fn nested_reservations_refund_their_source_regardless_of_drop_order() {
    for (consume, drop_parent_first) in [(false, false), (false, true), (true, false), (true, true)]
    {
        // Arrange
        let budget = ProviderCallBudget::new(1);
        let parent = budget.reserve().expect("initial reservation");
        let child = parent.reserve().expect("transfer reserved credit");
        assert_eq!(budget.remaining(), 0);
        assert_eq!(parent.remaining(), 0);
        assert_eq!(child.remaining(), 1);

        // Act / Assert
        if consume {
            child.consume().expect("redeem nested credit");
        }
        if drop_parent_first {
            drop(parent);
            assert_eq!(budget.remaining(), 0);
            drop(child);
        } else {
            drop(child);
            assert_eq!(parent.remaining(), usize::from(!consume));
            assert_eq!(budget.remaining(), 0);
            drop(parent);
        }
        assert_eq!(budget.remaining(), usize::from(!consume));
    }
}

#[test]
fn unused_clones_retain_credit_until_the_last_owner_drops() {
    // Arrange
    let budget = ProviderCallBudget::new(1);
    let reservation = budget.reserve().expect("reserve");
    let clone = reservation.clone();

    // Act / Assert
    drop(reservation);
    assert_eq!(budget.remaining(), 0);
    assert_eq!(clone.remaining(), 1);
    clone
        .ensure_available()
        .expect("reserved call is available");
    drop(clone);
    assert_eq!(budget.remaining(), 1);
}

#[test]
fn concurrent_clones_can_redeem_the_reserved_call_only_once() {
    // Arrange
    let budget = ProviderCallBudget::new(1);
    let reservation = budget.reserve().expect("reserve");

    // Act
    let attempts: usize = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let clone = reservation.clone();
                scope.spawn(move || usize::from(clone.consume().is_ok()))
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("consumer"))
            .sum()
    });

    // Assert
    assert_eq!(attempts, 1);
    assert_eq!(reservation.remaining(), 0);
    drop(reservation);
    assert_eq!(budget.remaining(), 0);
    assert!(budget.reserve().is_err());
}
