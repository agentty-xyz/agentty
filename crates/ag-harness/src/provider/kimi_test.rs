//! Unit tests for the Kimi provider, split by behavior.

#[path = "kimi_test/failure_test.rs"]
mod failure;
#[path = "kimi_test/reasoning_test.rs"]
mod reasoning;
#[path = "kimi_test/response_test.rs"]
mod response;
#[path = "kimi_test/support_test.rs"]
mod support;
#[path = "kimi_test/tool_test.rs"]
mod tool;

use super::{
    KIMI_K2_6, KIMI_K2_6_ESTIMATOR, KIMI_K3, KIMI_K3_ESTIMATOR, max_output_tokens, profile,
};

#[test]
fn declares_documented_windows_and_output_reservations() {
    // Arrange / Act
    let k3 = profile(KIMI_K3).expect("Kimi K3 profile");
    let k2_6 = profile(KIMI_K2_6).expect("Kimi K2.6 profile");

    // Assert
    // Window, reserved tokens, and input budget match the proactive
    // compaction budgets Agentty declared before the profiles moved here.
    assert_eq!(
        (
            k3.context_window_tokens(),
            k3.reserved_tokens(),
            k3.input_token_budget()
        ),
        (1_048_576, 223_112, 825_464)
    );
    assert_eq!(
        (
            k2_6.context_window_tokens(),
            k2_6.reserved_tokens(),
            k2_6.input_token_budget()
        ),
        (262_144, 55_778, 206_366)
    );
    assert_eq!(k3.estimator(), KIMI_K3_ESTIMATOR);
    assert_eq!(k2_6.estimator(), KIMI_K2_6_ESTIMATOR);
    assert_ne!(KIMI_K3_ESTIMATOR, KIMI_K2_6_ESTIMATOR);
    assert_eq!(max_output_tokens(KIMI_K3), Some(131_072));
    assert_eq!(max_output_tokens(KIMI_K2_6), Some(32_768));
    assert_eq!(profile("kimi-k2.7-code"), None);
    assert_eq!(max_output_tokens("kimi-k2.7-code"), None);
}
