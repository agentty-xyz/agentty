//! Process boundary of the trusted Cargo cache that native sandboxed
//! sessions copy from: fetching a worktree's locked crates.io packages
//! outside the sandbox.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ag_worker::TrustedCargoCache;
use async_trait::async_trait;

/// Called when a refresh starts fetching.
pub(crate) type FetchStarted = Box<dyn FnOnce() + Send>;

/// Capability for refreshing one project's trusted Cargo cache.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait CargoCacheClient: Send + Sync {
    /// Fetches the crates.io packages that `worktree`'s `Cargo.lock` pins
    /// and the cache of the project whose main checkout is `main_checkout`
    /// lacks, calling `on_fetch` first when anything needs fetching.
    ///
    /// # Errors
    /// Returns an error when the fetch fails; its details are for logs only.
    async fn refresh(
        &self,
        main_checkout: PathBuf,
        worktree: PathBuf,
        on_fetch: FetchStarted,
    ) -> Result<(), String>;
}

/// Refreshes caches under one cache root with the user's own environment,
/// Cargo executable, and Cargo configuration.
pub(crate) struct RealCargoCacheClient {
    cache_root: PathBuf,
    /// Cargo executable, resolved through the user's `PATH` when bare.
    cargo: String,
}

impl RealCargoCacheClient {
    /// Keeps project caches under `cache_root`, fetching with `cargo`.
    pub(crate) fn new(cache_root: PathBuf, cargo: &str) -> Self {
        Self {
            cache_root,
            cargo: cargo.to_string(),
        }
    }
}

#[async_trait]
impl CargoCacheClient for RealCargoCacheClient {
    async fn refresh(
        &self,
        main_checkout: PathBuf,
        worktree: PathBuf,
        on_fetch: FetchStarted,
    ) -> Result<(), String> {
        let environment = std::env::vars_os()
            .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
            .collect::<BTreeMap<_, _>>();

        TrustedCargoCache::for_project(&self.cache_root, &main_checkout)
            .refresh(&self.cargo, &worktree, &environment, on_fetch)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "cargo_cache_test.rs"]
mod tests;
