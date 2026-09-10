//! Public contract coverage for shared provider-turn accounting.

use ag_agent::{ProviderCallBudget, is_input_size_error};

#[test]
fn clones_share_an_atomic_limit_and_availability_checks_do_not_charge() {
    // Arrange
    let budget = ProviderCallBudget::new(64);
    budget.ensure_available().expect("initial capacity");
    budget
        .ensure_available()
        .expect("inspection does not charge");

    // Act
    let calls: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let budget = budget.clone();
                scope.spawn(move || (0..32).filter(|_| budget.consume().is_ok()).count())
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .sum()
    });

    // Assert
    assert_eq!(calls, 64);
    assert!(is_input_size_error(
        &budget
            .ensure_available()
            .expect_err("exhausted")
            .to_string()
    ));
    assert!(budget.consume().is_err());
}
