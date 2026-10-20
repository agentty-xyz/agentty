//! Shared in-memory repository fixtures exposed through `test-utils`.
//!
//! Test runners such as `cargo-nextest` start one process per test, so the
//! migrated schema is cached on disk rather than in process memory. Each
//! fixture deserializes that snapshot instead of replaying every migration.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteOwnedBuf, SqlitePoolOptions};

use crate::connection::{Database, MIGRATOR, in_memory_connect_options, open_in_memory_pool};
use crate::repository::{AppRepositories, RepositorySet};
use crate::timestamp::system_timestamp_source;

impl Database {
    /// Opens an isolated in-memory database initialized from the shared
    /// migrated-schema snapshot.
    ///
    /// # Errors
    /// Returns an error if the snapshot cannot be created, read, or loaded.
    #[doc(hidden)]
    pub async fn in_memory() -> Result<Self, crate::DbError> {
        let pool = open_snapshot_pool().await?;

        Ok(Self::from_pool(pool, system_timestamp_source()))
    }
}

impl AppRepositories {
    /// Replaces only task persistence for deterministic host admission tests.
    #[doc(hidden)]
    #[must_use]
    pub fn with_task_repository(&self, task: Arc<dyn crate::TaskRepository>) -> Self {
        let existing = &self.repositories;

        Self {
            repositories: Arc::new(RepositorySet {
                activity: Arc::clone(&existing.activity),
                operation: Arc::clone(&existing.operation),
                project: Arc::clone(&existing.project),
                review: Arc::clone(&existing.review),
                run: Arc::clone(&existing.run),
                session: Arc::clone(&existing.session),
                setting: Arc::clone(&existing.setting),
                task,
                usage: Arc::clone(&existing.usage),
            }),
        }
    }

    /// Opens an isolated in-memory repository bundle for tests.
    ///
    /// # Errors
    /// Returns an error if the snapshot cannot be created, read, or loaded.
    #[doc(hidden)]
    pub async fn in_memory() -> Result<Self, crate::DbError> {
        let (repositories, _pool) = Self::in_memory_with_pool().await?;

        Ok(repositories)
    }

    /// Opens an isolated in-memory repository bundle plus its shared
    /// `SQLite` pool for tests that need raw SQL setup.
    ///
    /// # Errors
    /// Returns an error if the snapshot cannot be created, read, or loaded.
    #[doc(hidden)]
    pub async fn in_memory_with_pool() -> Result<(Self, SqlitePool), crate::DbError> {
        let pool = open_snapshot_pool().await?;
        let repositories = Self::from_pool(pool.clone());

        Ok((repositories, pool))
    }

    /// Creates a repository bundle backed by one shared `SQLite` pool.
    pub(crate) fn from_pool(pool: SqlitePool) -> Self {
        Self::from_pool_and_timestamp_source(pool, system_timestamp_source())
    }
}

/// Opens a one-connection in-memory pool whose schema is the migrated
/// snapshot, keeping the regular connection settings.
async fn open_snapshot_pool() -> Result<SqlitePool, crate::DbError> {
    let snapshot: Arc<[u8]> = load_or_create_snapshot().await?.into();
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .after_connect(move |connection, _| {
            let snapshot = Arc::clone(&snapshot);

            Box::pin(async move {
                let data = SqliteOwnedBuf::try_from(&snapshot[..])?;

                connection.deserialize(None, data, false).await
            })
        })
        .connect_with(in_memory_connect_options())
        .await?;

    Ok(pool)
}

/// Reads the snapshot for the current migrations, creating and publishing it
/// atomically when no test process has done so yet.
async fn load_or_create_snapshot() -> Result<Vec<u8>, crate::DbError> {
    let path = snapshot_path();
    if let Ok(snapshot) = tokio::fs::read(&path).await
        && !snapshot.is_empty()
    {
        return Ok(snapshot);
    }
    let pool = open_in_memory_pool(1).await?;
    let snapshot = pool.acquire().await?.serialize(None).await?.to_vec();
    let staging = path.with_extension(format!("{}.tmp", std::process::id()));
    tokio::fs::write(&staging, &snapshot).await?;
    tokio::fs::rename(&staging, &path).await?;

    Ok(snapshot)
}

/// Names the snapshot after every migration version and checksum, so a
/// schema change never reuses a stale snapshot.
fn snapshot_path() -> PathBuf {
    let mut hasher = DefaultHasher::new();
    for migration in MIGRATOR.iter() {
        migration.version.hash(&mut hasher);
        migration.checksum.hash(&mut hasher);
    }

    std::env::temp_dir().join(format!("ag-store-schema-{:016x}.sqlite", hasher.finish()))
}
