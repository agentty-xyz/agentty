use crate::domain::agent::AgentModel;
use crate::domain::harness::HarnessAvailability;

#[test]
/// Hides the row without the flag, disables it without credentials, and
/// otherwise starts on the configured provider's model.
fn harness_availability_follows_flag_and_credentials() {
    // Arrange
    let cases = [
        (false, None, HarnessAvailability::Hidden, false, None),
        (
            false,
            Some(AgentModel::KimiK3),
            HarnessAvailability::Hidden,
            false,
            None,
        ),
        (
            true,
            None,
            HarnessAvailability::MissingCredentials,
            true,
            None,
        ),
        (
            true,
            Some(AgentModel::KimiK3),
            HarnessAvailability::Available(AgentModel::KimiK3),
            true,
            Some(AgentModel::KimiK3),
        ),
    ];

    // Act / Assert
    for (flag, configured_model, expected, visible, default_model) in cases {
        let availability = HarnessAvailability::resolve(flag, configured_model);
        assert_eq!(availability, expected);
        assert_eq!(availability.is_visible(), visible);
        assert_eq!(availability.is_available(), default_model.is_some());
        assert_eq!(availability.default_model(), default_model);
    }
    assert_eq!(HarnessAvailability::default(), HarnessAvailability::Hidden);
}
