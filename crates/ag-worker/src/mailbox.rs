use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as AdmissionMutex};

use tokio::sync::{Mutex, Notify, OwnedMutexGuard, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{WorkerHost, scheduler};

/// Submission handle for a worker-owned serial mailbox.
///
/// Clones share command delivery, wakeups, and the ordering source used by
/// host-owned message queues. Dropping the final handle closes the mailbox;
/// the worker settles remaining work and invokes the host's shutdown hook.
pub struct SessionWorkerHandle<C> {
    admission: Arc<AdmissionMutex<()>>,
    completion: watch::Receiver<()>,
    execution: Arc<Mutex<()>>,
    queued_work_sequence: Arc<AtomicU64>,
    sender: mpsc::UnboundedSender<C>,
    stop: CancellationToken,
    wakeup: Arc<Notify>,
}

impl<C: Send + 'static> SessionWorkerHandle<C> {
    /// Starts serial execution with host-defined policy and ordered effects.
    pub fn spawn<H>(host: H, queued_work_sequence: Arc<AtomicU64>) -> Self
    where
        H: WorkerHost<Command = C> + 'static,
    {
        let (sender, receiver) = mpsc::unbounded_channel();
        let wakeup = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let execution = Arc::new(Mutex::new(()));
        let (completed, completion) = watch::channel(());
        tokio::spawn({
            let wakeup = Arc::clone(&wakeup);
            let stop = stop.clone();
            let execution = Arc::clone(&execution);
            async move {
                scheduler::run_until_stopped(host, wakeup, receiver, stop, execution).await;
                drop(completed);
            }
        });

        Self {
            admission: Arc::default(),
            completion,
            execution,
            queued_work_sequence,
            sender,
            stop,
            wakeup,
        }
    }

    /// Submits a command whose persistence and scheduling metadata are ready.
    ///
    /// # Errors
    /// Returns the command once shutdown has been requested or the worker has
    /// stopped.
    pub fn submit(&self, command: C) -> Result<(), mpsc::error::SendError<C>> {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stop.is_cancelled() {
            return Err(mpsc::error::SendError(command));
        }

        self.sender.send(command)
    }

    /// Reserves the next order shared with host-owned queued messages.
    pub fn next_queued_work_order(&self) -> u64 {
        self.queued_work_sequence.fetch_add(1, Ordering::Relaxed)
    }

    /// Reconsiders buffered work after host pause state or messages change.
    pub fn wake(&self) {
        self.wakeup.notify_one();
    }

    /// Waits for in-flight effects, then holds scheduling without removing any
    /// pending work. Dropping the returned guard resumes the same mailbox.
    pub async fn pause(&self) -> SessionWorkerPause {
        SessionWorkerPause {
            admission: Arc::clone(&self.admission),
            completion: self.completion.clone(),
            execution: Arc::clone(&self.execution).lock_owned().await,
            stop: self.stop.clone(),
        }
    }

    /// Stops scheduling, lets the in-flight workflow finish, and abandons all
    /// pending work. Returns only after host cleanup finishes. Other handle
    /// clones cannot keep the worker alive or resume its queue.
    pub async fn shutdown(mut self) {
        {
            let _admission = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.stop.cancel();
        }
        let _ = self.completion.changed().await;
    }
}

impl<C> Clone for SessionWorkerHandle<C> {
    fn clone(&self) -> Self {
        Self {
            admission: Arc::clone(&self.admission),
            completion: self.completion.clone(),
            execution: Arc::clone(&self.execution),
            queued_work_sequence: Arc::clone(&self.queued_work_sequence),
            sender: self.sender.clone(),
            stop: self.stop.clone(),
            wakeup: Arc::clone(&self.wakeup),
        }
    }
}

/// Reversible hold on a worker's scheduling and cleanup.
///
/// Drop this guard to resume work after a failed host update, or consume it
/// with [`Self::shutdown`] after the update commits.
pub struct SessionWorkerPause {
    admission: Arc<AdmissionMutex<()>>,
    completion: watch::Receiver<()>,
    execution: OwnedMutexGuard<()>,
    stop: CancellationToken,
}

impl SessionWorkerPause {
    /// Retires the paused worker without letting pending work start between
    /// releasing the hold and requesting shutdown. Waits for host cleanup.
    pub async fn shutdown(mut self) {
        {
            let _admission = self
                .admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.stop.cancel();
        }
        drop(self.execution);
        let _ = self.completion.changed().await;
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[path = "mailbox_support_test.rs"]
mod support;
#[cfg(any(test, feature = "test-utils"))]
pub use support::test_session_worker_handle;

#[cfg(test)]
#[path = "mailbox_test.rs"]
mod tests;
