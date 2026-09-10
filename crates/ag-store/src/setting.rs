//! Setting-scoped persistence adapters and query helpers.

use ag_agent::{ReasoningLevel, ResponseStyle, SpeedMode};
use ag_session::SettingName;
use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::DbError;

/// Settings-focused persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait SettingRepository: Send + Sync {
    /// Looks up one project-scoped setting value by project and name.
    async fn get_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<Option<String>, DbError>;

    /// Looks up a setting value by name.
    async fn get_setting(&self, name: SettingName) -> Result<Option<String>, DbError>;

    /// Loads the active project identifier from application settings.
    async fn load_active_project_id(&self) -> Result<Option<i64>, DbError>;

    /// Loads one project-scoped reasoning-effort setting.
    async fn load_project_reasoning_level(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<ReasoningLevel, DbError>;

    /// Loads the project-scoped default response style.
    async fn load_project_response_style(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<ResponseStyle, DbError>;

    /// Loads one project-scoped response-speed setting.
    async fn load_project_speed_mode(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<SpeedMode, DbError>;

    /// Persists the active project identifier in application settings.
    async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError>;

    /// Inserts or updates one project-scoped setting by project and name.
    async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
        value: &str,
    ) -> Result<(), DbError>;

    /// Inserts or updates project-scoped settings as one transaction.
    async fn upsert_project_settings(
        &self,
        project_id: i64,
        settings: Vec<(SettingName, String)>,
    ) -> Result<(), DbError>;

    /// Inserts or updates a setting by name.
    async fn upsert_setting(&self, name: SettingName, value: &str) -> Result<(), DbError>;
}

/// `SQLite` implementation of [`SettingRepository`].
#[derive(Clone)]
pub(crate) struct SqliteSettingRepository(SqlitePool);

impl SqliteSettingRepository {
    /// Creates a settings repository backed by the provided pool.
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self(pool)
    }
}

#[async_trait]
impl SettingRepository for SqliteSettingRepository {
    async fn get_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<Option<String>, DbError> {
        let setting_name = name.as_str();
        let row = sqlx::query_as!(
            RequiredSettingValueRow,
            r#"
SELECT value AS "value!: _"
FROM project_setting
WHERE project_id = ?
  AND name = ?
"#,
            project_id,
            setting_name
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| row.value))
    }

    async fn get_setting(&self, name: SettingName) -> Result<Option<String>, DbError> {
        let setting_name = name.as_str();
        let row = sqlx::query_as!(
            RequiredSettingValueRow,
            r#"
SELECT value AS "value!: _"
FROM setting
WHERE name = ?
"#,
            setting_name
        )
        .fetch_optional(&self.0)
        .await?;

        Ok(row.map(|row| row.value))
    }

    async fn load_active_project_id(&self) -> Result<Option<i64>, DbError> {
        let setting_value = self.get_setting(SettingName::ActiveProjectId).await?;

        Ok(setting_value.and_then(|value| value.parse::<i64>().ok()))
    }

    async fn load_project_reasoning_level(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<ReasoningLevel, DbError> {
        let setting_value = self.get_project_setting(project_id, name).await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    async fn load_project_response_style(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<ResponseStyle, DbError> {
        let setting_value = self.get_project_setting(project_id, name).await?;

        let response_style = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ResponseStyle>().ok())
            .unwrap_or_default();

        Ok(response_style)
    }

    async fn load_project_speed_mode(
        &self,
        project_id: i64,
        name: SettingName,
    ) -> Result<SpeedMode, DbError> {
        let setting_value = self.get_project_setting(project_id, name).await?;

        let speed_mode = setting_value
            .as_deref()
            .and_then(|value| value.parse::<SpeedMode>().ok())
            .unwrap_or_default();

        Ok(speed_mode)
    }

    async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError> {
        self.upsert_setting(SettingName::ActiveProjectId, &project_id.to_string())
            .await
    }

    async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
        value: &str,
    ) -> Result<(), DbError> {
        sqlx::query!(
            r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
            project_id,
            name.as_str(),
            value
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }

    async fn upsert_project_settings(
        &self,
        project_id: i64,
        settings: Vec<(SettingName, String)>,
    ) -> Result<(), DbError> {
        let mut transaction = self.0.begin().await?;

        for (name, value) in settings {
            sqlx::query!(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
                project_id,
                name.as_str(),
                value
            )
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn upsert_setting(&self, name: SettingName, value: &str) -> Result<(), DbError> {
        sqlx::query!(
            r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
            name.as_str(),
            value
        )
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

/// Scalar row used to return one required setting value.
struct RequiredSettingValueRow {
    value: String,
}

#[cfg(test)]
#[path = "setting_test.rs"]
mod tests;
