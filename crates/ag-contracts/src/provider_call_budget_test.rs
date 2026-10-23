use crate::ProviderCallBudget;

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
