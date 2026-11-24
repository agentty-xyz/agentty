//! Background refreshes of the Cargo cache that native sandboxed Harness
//! sessions copy from.
//!
//! When a Harness session's worktree has a `Cargo.lock` naming crates.io
//! packages its project's cache lacks, Agentty fetches them outside the
//! sandbox, one refresh at a time per project, and tells the user with a
//! transient notice that never reaches the model. The next turn's sandbox
//! copies what the fetch stored. A failed fetch only costs the session its
//! own downloads, so it never fails a turn.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use ag_session::{AgentKind, ResolvedEnv};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::{AppEvent, spawn};
use crate::domain::session::SessionId;
use crate::infra::cargo_cache::CargoCacheClient;

/// Notice shown while the project's Cargo dependencies are fetched.
const CACHING_NOTICE: &str =
    "Caching Cargo dependencies in the background for the next turn's commands.";
/// Notice shown when the fetch fails.
const FAILED_NOTICE: &str = "Caching Cargo dependencies failed; commands download what they need.";

/// Whether a refresh tells the session what it does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Announcement {
    /// Shows when it fetches and when it fails, unless the project's last
    /// announced refresh already failed.
    Notices,
    /// Stays silent, as a head start before the session's first turn.
    Silent,
}

/// Starts trusted Cargo cache refreshes for Harness sessions.
#[derive(Clone, Default)]
pub(crate) struct CargoCacheWarmer {
    /// Refreshes caches outside the sandbox; `None` disables the cache.
    client: Option<Arc<dyn CargoCacheClient>>,
    /// Main checkouts whose last announced refresh failed, whose retries
    /// stay silent until one succeeds.
    failing: Arc<Mutex<HashSet<PathBuf>>>,
    /// One lock per main checkout, so refreshes of a project run one at a
    /// time and a later one sees what an earlier one fetched.
    projects: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl CargoCacheWarmer {
    /// Refreshes caches through `client`.
    pub(crate) fn new(client: Arc<dyn CargoCacheClient>) -> Self {
        Self {
            client: Some(client),
            ..Self::default()
        }
    }

    /// Returns whether this launch keeps a trusted Cargo cache.
    pub(crate) fn is_enabled(&self) -> bool {
        self.client.is_some()
    }

    /// Starts a background refresh of the cache of `env`'s project for a
    /// session of `agent_kind`, after any refresh of the same project ends,
    /// unless the session does not run Harness turns, its project has no
    /// main checkout, or the cache is disabled. With
    /// [`Announcement::Notices`], the refresh tells session `session_id`
    /// through `app_event_tx` when it fetches and when it fails; a cache
    /// that already covers the worktree stays silent.
    pub(crate) fn warm(
        &self,
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        session_id: &str,
        agent_kind: AgentKind,
        env: &ResolvedEnv,
        announcement: Announcement,
    ) -> Option<JoinHandle<()>> {
        if agent_kind != AgentKind::Harness {
            return None;
        }
        let client = Arc::clone(self.client.as_ref()?);
        let main_checkout = env.main_checkout.clone()?;
        let project = Arc::clone(
            self.projects
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(main_checkout.clone())
                .or_default(),
        );
        let failing = Arc::clone(&self.failing);
        let worktree = env.folder.clone();
        let app_event_tx = app_event_tx.clone();
        let session_id = SessionId::from(session_id);

        Some(spawn::spawn_traced(async move {
            let _project = project.lock().await;
            let announces = announcement == Announcement::Notices
                && !Self::lock(&failing).contains(&main_checkout);
            let on_fetch = {
                let app_event_tx = app_event_tx.clone();
                let session_id = session_id.clone();

                Box::new(move || {
                    if announces {
                        Self::notify(&app_event_tx, session_id, CACHING_NOTICE);
                    }
                })
            };
            let outcome = client
                .refresh(main_checkout.clone(), worktree, on_fetch)
                .await;
            if announcement == Announcement::Silent {
                return;
            }
            if let Err(error) = outcome {
                tracing::warn!(session_id = %session_id, "Cargo cache refresh failed: {error}");
                if announces {
                    Self::notify(&app_event_tx, session_id, FAILED_NOTICE);
                }
                Self::lock(&failing).insert(main_checkout);
            } else {
                Self::lock(&failing).remove(&main_checkout);
            }
        }))
    }

    /// Locks the set of failing projects; it stays valid after a panic.
    fn lock(failing: &Mutex<HashSet<PathBuf>>) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        failing.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Shows `notice` in session `session_id` until its next turn.
    fn notify(app_event_tx: &mpsc::UnboundedSender<AppEvent>, session_id: SessionId, notice: &str) {
        // The reducer may already be gone during shutdown.
        let _ = app_event_tx.send(AppEvent::SessionWorkflowNoticeUpdated {
            notice: notice.to_string(),
            session_id,
        });
    }
}

#[cfg(test)]
#[path = "cargo_cache_test.rs"]
mod tests;
