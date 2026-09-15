pub use ag_runtime::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};

/// Codex app-server service-tier value for this speed mode.
pub(crate) const fn codex_service_tier(mode: SpeedMode) -> &'static str {
    match mode {
        SpeedMode::Normal => "default",
        SpeedMode::Fast => "fast",
    }
}

/// Whether Claude Code should enable its `fastMode` setting.
pub(crate) const fn claude_fast_mode(mode: SpeedMode) -> bool {
    matches!(mode, SpeedMode::Fast)
}

#[cfg(test)]
#[path = "session_test.rs"]
mod tests;
