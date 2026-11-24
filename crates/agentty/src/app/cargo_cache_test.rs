//! Trusted Cargo cache refresh scheduling and notice tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_session::{AgentKind, EnvId, ResolvedEnv};
use async_trait::async_trait;
use tokio::sync::{Semaphore, mpsc};

use super::{Announcement, CACHING_NOTICE, CargoCacheWarmer, FAILED_NOTICE};
use crate::app::AppEvent;
use crate::infra::cargo_cache::{CargoCacheClient, FetchStarted, MockCargoCacheClient};

/// Returns the resolved Env of a session worktree of `main_checkout`.
fn session_env(main_checkout: Option<&Path>) -> ResolvedEnv {
    ResolvedEnv {
        branch: "wt/session".to_string(),
        folder: PathBuf::from("/worktrees/session"),
        id: EnvId::SessionWorktree("session".into()),
        main_checkout: main_checkout.map(Path::to_path_buf),
    }
}

/// Returns the workflow notices `events` carried for `session`.
fn notices(events: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<String> {
    let mut notices = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::SessionWorkflowNoticeUpdated { notice, session_id } = event {
            assert_eq!(session_id.as_str(), "session");
            notices.push(notice);
        }
    }

    notices
}

#[tokio::test]
/// Refreshes nothing for sessions of other agents, worktrees without a main
/// checkout, or a launch without a cache.
async fn warm_skips_sessions_the_cache_does_not_serve() {
    // Arrange
    let client = MockCargoCacheClient::new();
    let warmer = CargoCacheWarmer::new(Arc::new(client));
    let disabled = CargoCacheWarmer::default();
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let env = session_env(Some(Path::new("/projects/main")));
    let notices_requested = Announcement::Notices;

    // Act
    let codex = warmer.warm(
        &events_tx,
        "session",
        AgentKind::Codex,
        &env,
        notices_requested,
    );
    let bare = warmer.warm(
        &events_tx,
        "session",
        AgentKind::Harness,
        &session_env(None),
        notices_requested,
    );
    let off = disabled.warm(
        &events_tx,
        "session",
        AgentKind::Harness,
        &env,
        notices_requested,
    );

    // Assert
    assert!(codex.is_none() && bare.is_none() && off.is_none());
    assert!(warmer.is_enabled());
    assert!(!disabled.is_enabled());
    assert_eq!(notices(&mut events_rx), Vec::<String>::new());
}

#[tokio::test]
/// Stays silent when the cache covers the worktree, announces a fetch,
/// reports a failure once, retries silently while the project keeps
/// failing, and announces again after a success; a silent head start
/// shows nothing and leaves that state alone.
async fn warm_announces_fetches_and_reports_failures_once() {
    // Arrange
    let main_checkout = PathBuf::from("/projects/main");
    let worktree = PathBuf::from("/worktrees/session");
    let mut client = MockCargoCacheClient::new();
    let failure = || Err("failed".to_string());
    let mut outcomes = [
        (false, Ok(())),
        (true, Ok(())),
        (true, failure()),
        (true, failure()),
        (true, Ok(())),
        (true, failure()),
        (true, Ok(())),
    ]
    .into_iter();
    client
        .expect_refresh()
        .withf(move |checkout, folder, _| *checkout == main_checkout && *folder == worktree)
        .times(7)
        .returning(move |_, _, on_fetch| {
            let (fetches, outcome) = outcomes.next().unwrap_or((false, Ok(())));
            if fetches {
                on_fetch();
            }

            outcome
        });
    let warmer = CargoCacheWarmer::new(Arc::new(client));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let env = session_env(Some(Path::new("/projects/main")));
    let mut observed = Vec::new();
    let announcements = [
        Announcement::Notices,
        Announcement::Notices,
        Announcement::Notices,
        Announcement::Notices,
        Announcement::Notices,
        Announcement::Silent,
        Announcement::Notices,
    ];

    // Act
    for announcement in announcements {
        warmer
            .warm(
                &events_tx,
                "session",
                AgentKind::Harness,
                &env,
                announcement,
            )
            .expect("refresh task")
            .await
            .expect("refresh");
        observed.push(notices(&mut events_rx));
    }

    // Assert
    let caching = CACHING_NOTICE.to_string();
    assert_eq!(
        observed,
        [
            Vec::new(),
            vec![caching.clone()],
            vec![caching.clone(), FAILED_NOTICE.to_string()],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![caching],
        ]
    );
}

/// A cache client whose refreshes wait until the test releases them.
struct HeldRefresh {
    /// Permits that let waiting refreshes finish.
    release: Semaphore,
    /// One permit per refresh that started.
    started: Semaphore,
}

impl HeldRefresh {
    /// Waits until one more refresh started.
    async fn wait_for_start(&self) {
        self.started
            .acquire()
            .await
            .expect("open semaphore")
            .forget();
    }
}

#[async_trait]
impl CargoCacheClient for HeldRefresh {
    async fn refresh(
        &self,
        _main_checkout: PathBuf,
        _worktree: PathBuf,
        _on_fetch: FetchStarted,
    ) -> Result<(), String> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .map_err(|error| error.to_string())?
            .forget();

        Ok(())
    }
}

#[tokio::test]
/// Runs one refresh per project at a time: a second refresh of the same
/// project starts only once the first ends, while another project's
/// refresh runs meanwhile.
async fn warm_runs_one_refresh_per_project_at_a_time() {
    // Arrange
    let client = Arc::new(HeldRefresh {
        release: Semaphore::new(0),
        started: Semaphore::new(0),
    });
    let warmer = CargoCacheWarmer::new(Arc::clone(&client) as Arc<dyn CargoCacheClient>);
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let env = session_env(Some(Path::new("/projects/main")));
    let other = session_env(Some(Path::new("/projects/other")));
    let warm = |env: &ResolvedEnv| {
        warmer
            .warm(
                &events_tx,
                "session",
                AgentKind::Harness,
                env,
                Announcement::Silent,
            )
            .expect("refresh task")
    };

    // Act
    let first = warm(&env);
    client.wait_for_start().await;
    let queued = warm(&env);
    let other_project = warm(&other);
    client.wait_for_start().await;
    let queued_started_early = client.started.try_acquire().is_ok();
    client.release.add_permits(2);
    first.await.expect("first refresh ends");
    other_project.await.expect("other refresh ends");
    client.wait_for_start().await;
    client.release.add_permits(1);
    queued.await.expect("queued refresh ends");

    // Assert
    assert!(!queued_started_early, "the same project waits its turn");
}
