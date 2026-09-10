//! Agentty persistence facade and database-location policy.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

pub use ag_store::*;

use crate::infra::clock;

/// Subdirectory under the Agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default Agentty database filename.
pub const DB_FILE: &str = "agentty.db";

/// Acquires exclusive application ownership of one Agentty root.
///
/// Keep the returned file open until the application stops. The OS releases
/// the lock when the handle closes or the process exits; the lock file must
/// remain in place so concurrent starts always lock the same file.
/// Acquire this before opening the database or running startup recovery.
///
/// # Errors
/// Returns [`io::ErrorKind::WouldBlock`] when another instance owns this root,
/// or an I/O error when the lock directory or file cannot be opened or locked.
pub async fn acquire_instance_lock(root: &Path) -> io::Result<File> {
    let directory = root.join(DB_DIR);
    tokio::fs::create_dir_all(&directory).await?;
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("agentty.lock"))
        .await?
        .into_std()
        .await;
    file.try_lock()?;

    Ok(file)
}

/// Returns a store timestamp source backed by Agentty's environment-selected
/// clock.
///
/// Feature tests pin that clock so database ordering and activity timestamps
/// remain deterministic alongside rendered frame time.
pub fn timestamp_source_from_environment() -> Arc<dyn TimestampSource> {
    let clock = clock::from_environment();

    Arc::new(move || clock::unix_timestamp_seconds(clock.as_ref()))
}

#[cfg(test)]
#[path = "db_test.rs"]
mod tests;
