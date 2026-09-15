use ag_runtime::SpeedMode;

use crate::model::session;

#[test]
fn speed_mode_maps_provider_settings() {
    // Arrange, Act, Assert
    assert_eq!(session::codex_service_tier(SpeedMode::Normal), "default");
    assert!(!session::claude_fast_mode(SpeedMode::Normal));
    assert_eq!(session::codex_service_tier(SpeedMode::Fast), "fast");
    assert!(session::claude_fast_mode(SpeedMode::Fast));
}
