//! Database and worktree fixtures shared by integration tests.

use std::path::{Path, PathBuf};

use ag_session::SettingName;
use agentty::app;
use agentty::db::{Database, DbError};
use agentty::domain::agent::ReasoningLevel;

/// Returns the canonical session folder path for integration-test fixtures.
pub(crate) fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    base.join(&session_id[..session_id.len().min(8)])
}

/// Persists the active project id for integration-test database setup.
pub(crate) async fn persist_active_project_id_for_test(
    database: &Database,
    project_id: i64,
) -> Result<(), DbError> {
    sqlx::query!(
        r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        SettingName::ActiveProjectId.as_str(),
        project_id.to_string()
    )
    .execute(database.pool())
    .await?;

    Ok(())
}

/// Persists the active list tab for integration-test database setup.
pub(crate) async fn persist_active_tab_for_test(
    database: &Database,
    tab: app::Tab,
) -> Result<(), DbError> {
    sqlx::query!(
        r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        SettingName::ActiveTab.as_str(),
        tab.title()
    )
    .execute(database.pool())
    .await?;

    Ok(())
}

/// Persists the three project role reasoning defaults for integration-test
/// database setup using canonical `SettingName` keys.
pub(crate) async fn persist_project_reasoning_levels_for_test(
    database: &Database,
    project_id: i64,
    smart_reasoning_level: ReasoningLevel,
    fast_reasoning_level: ReasoningLevel,
    review_reasoning_level: ReasoningLevel,
) -> Result<(), DbError> {
    database
        .settings()
        .upsert_project_settings(
            project_id,
            vec![
                (
                    SettingName::DefaultSmartReasoningLevel,
                    smart_reasoning_level.as_str().to_string(),
                ),
                (
                    SettingName::DefaultFastReasoningLevel,
                    fast_reasoning_level.as_str().to_string(),
                ),
                (
                    SettingName::DefaultReviewReasoningLevel,
                    review_reasoning_level.as_str().to_string(),
                ),
            ],
        )
        .await
}

#[cfg(test)]
#[path = "database_test.rs"]
mod tests;
