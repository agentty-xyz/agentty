//! Shared in-memory repository fixtures exposed through `test-utils`.

use sqlx::SqlitePool;

use crate::connection::open_in_memory_pool;
use crate::repository::AppRepositories;
use crate::timestamp::system_timestamp_source;

impl AppRepositories {
    /// Opens an isolated in-memory repository bundle for tests.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    #[doc(hidden)]
    pub async fn in_memory() -> Result<Self, crate::DbError> {
        let (repositories, _pool) = Self::in_memory_with_pool().await?;

        Ok(repositories)
    }

    /// Opens an isolated in-memory repository bundle plus its shared
    /// `SQLite` pool for tests that need raw SQL setup.
    ///
    /// # Errors
    /// Returns an error if the database connection or migrations fail.
    #[doc(hidden)]
    pub async fn in_memory_with_pool() -> Result<(Self, SqlitePool), crate::DbError> {
        let pool = open_in_memory_pool(1).await?;
        let repositories = Self::from_pool(pool.clone());

        Ok((repositories, pool))
    }

    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_pool(pool: SqlitePool) -> Self {
        Self::from_pool_and_timestamp_source(pool, system_timestamp_source())
    }
}
