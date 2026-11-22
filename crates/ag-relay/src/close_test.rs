use axum::extract::ws::close_code;

use super::CloseReason;

/// Every reason, in declaration order.
const REASONS: [CloseReason; 13] = [
    CloseReason::HandshakeFailed,
    CloseReason::InstanceBusy,
    CloseReason::NoAnswer,
    CloseReason::PeerLeft,
    CloseReason::RateLimited,
    CloseReason::RelayFull,
    CloseReason::RelayStopped,
    CloseReason::Replaced,
    CloseReason::TextMessage,
    CloseReason::TooManyInstances,
    CloseReason::TooManyStreams,
    CloseReason::Unavailable,
    CloseReason::UnknownInstance,
];

#[test]
fn every_reason_has_distinct_text() {
    // Arrange
    let mut texts: Vec<&str> = REASONS.iter().map(|reason| reason.text()).collect();

    // Act
    texts.sort_unstable();
    texts.dedup();

    // Assert
    assert_eq!(texts.len(), REASONS.len());
    assert!(texts.iter().all(|text| !text.is_empty()));
}

#[test]
fn close_codes_separate_leaving_stopping_and_policy_reasons() {
    // Arrange
    let reasons = [
        CloseReason::PeerLeft,
        CloseReason::NoAnswer,
        CloseReason::RelayStopped,
        CloseReason::Unavailable,
        CloseReason::RateLimited,
    ];

    // Act
    let codes = reasons.map(CloseReason::code);

    // Assert
    assert_eq!(
        codes,
        [
            close_code::NORMAL,
            close_code::AWAY,
            close_code::AWAY,
            close_code::AWAY,
            close_code::POLICY
        ]
    );
}

#[test]
fn random_source_failures_report_the_relay_as_unavailable() {
    // Arrange
    let failure = getrandom::Error::UNSUPPORTED;

    // Act
    let reason = CloseReason::from(failure);

    // Assert
    assert_eq!(reason, CloseReason::Unavailable);
}
