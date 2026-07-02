//! Setting-scoped persistence adapters and query helpers.

use async_trait::async_trait;
use sqlx::SqlitePool;

use crate::domain::agent::ReasoningLevel;
use crate::domain::setting::SettingName;
use crate::infra::db::DbError;

/// Settings-focused persistence boundary used by app orchestration and tests.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait SettingRepository: Send + Sync {
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
    ) -> Result<ReasoningLevel, DbError>;

    #[cfg(test)]
    /// Loads the persisted reasoning-effort setting.
    async fn load_reasoning_level(&self) -> Result<ReasoningLevel, DbError>;

    /// Persists the active project identifier in application settings.
    async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError>;

    /// Persists one project-scoped reasoning-effort setting.
    async fn set_project_reasoning_level(
        &self,
        project_id: i64,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError>;

    #[cfg(test)]
    /// Persists the global reasoning-effort setting.
    async fn set_reasoning_level(&self, reasoning_level: ReasoningLevel) -> Result<(), DbError>;

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

/// Scalar row used to return one required setting value.
struct RequiredSettingValueRow {
    value: String,
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
    ) -> Result<ReasoningLevel, DbError> {
        let setting_value = self
            .get_project_setting(project_id, SettingName::ReasoningLevel)
            .await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    #[cfg(test)]
    async fn load_reasoning_level(&self) -> Result<ReasoningLevel, DbError> {
        let setting_value = self.get_setting(SettingName::ReasoningLevel).await?;

        let reasoning_level = setting_value
            .as_deref()
            .and_then(|value| value.parse::<ReasoningLevel>().ok())
            .unwrap_or_default();

        Ok(reasoning_level)
    }

    async fn set_active_project_id(&self, project_id: i64) -> Result<(), DbError> {
        self.upsert_setting(SettingName::ActiveProjectId, &project_id.to_string())
            .await
    }

    async fn set_project_reasoning_level(
        &self,
        project_id: i64,
        reasoning_level: ReasoningLevel,
    ) -> Result<(), DbError> {
        self.upsert_project_setting(
            project_id,
            SettingName::ReasoningLevel,
            reasoning_level.codex(),
        )
        .await
    }

    #[cfg(test)]
    async fn set_reasoning_level(&self, reasoning_level: ReasoningLevel) -> Result<(), DbError> {
        self.upsert_setting(SettingName::ReasoningLevel, reasoning_level.codex())
            .await
    }

    async fn upsert_project_setting(
        &self,
        project_id: i64,
        name: SettingName,
        value: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
        )
        .bind(project_id)
        .bind(name.as_str())
        .bind(value)
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
            sqlx::query(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
            )
            .bind(project_id)
            .bind(name.as_str())
            .bind(value)
            .execute(&mut *transaction)
            .await?;
        }

        transaction.commit().await?;

        Ok(())
    }

    async fn upsert_setting(&self, name: SettingName, value: &str) -> Result<(), DbError> {
        sqlx::query(
            r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
        )
        .bind(name.as_str())
        .bind(value)
        .execute(&self.0)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::agent::AgentModel;
    use crate::infra::db::AppRepositories;

    #[tokio::test]
    /// Verifies project setting batches roll back when any setting write fails.
    async fn test_upsert_project_settings_rolls_back_partial_batch() {
        // Arrange
        let (repositories, pool) = AppRepositories::in_memory_with_pool().await;
        let project_id = repositories
            .projects()
            .upsert_project("/tmp/project", Some("main".to_string()))
            .await
            .expect("failed to insert project");
        sqlx::query(
            r"
CREATE TRIGGER fail_default_fast_agent_insert
BEFORE INSERT ON project_setting
WHEN NEW.name = 'DefaultFastAgent'
BEGIN
    SELECT RAISE(FAIL, 'forced setting failure');
END;
",
        )
        .execute(&pool)
        .await
        .expect("failed to install failure trigger");

        // Act
        let result = repositories
            .settings()
            .upsert_project_settings(
                project_id,
                vec![
                    (
                        SettingName::DefaultFastModel,
                        AgentModel::Gemini31ProPreview.as_str().to_string(),
                    ),
                    (SettingName::DefaultFastAgent, "antigravity".to_string()),
                ],
            )
            .await;

        // Assert
        assert!(result.is_err());
        assert_eq!(
            repositories
                .settings()
                .get_project_setting(project_id, SettingName::DefaultFastModel)
                .await
                .expect("failed to load fast model setting"),
            None
        );
        assert_eq!(
            repositories
                .settings()
                .get_project_setting(project_id, SettingName::DefaultFastAgent)
                .await
                .expect("failed to load fast agent setting"),
            None
        );
    }
}
