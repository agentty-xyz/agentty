//! Retained command controls and admission, independent of filesystem writes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;
use tokio::sync::{OwnedMutexGuard, watch};

use crate::SessionError;
use crate::command_journal::{CommandOutcome, CommandRecord};
use crate::execution::{ExecutionControl, ExecutionError};
use crate::session::WriteJournal;

/// Command cleanup or outcome recording has not settled. macOS success means
/// only its documented best-effort process-group scope, never all descendants.
#[derive(Clone, Debug, Error)]
#[error("command cleanup or outcome recording remains unresolved")]
pub struct CommandSettlementError;

#[derive(Clone)]
pub(crate) struct Commands(watch::Sender<State>);

impl Default for Commands {
    fn default() -> Self {
        Self(watch::Sender::new(State::default()))
    }
}

impl Commands {
    pub(crate) fn outcomes(&self) -> Vec<Option<CommandOutcome>> {
        self.0
            .borrow()
            .operations
            .iter()
            .map(|operation| {
                operation
                    .outcome
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
            .collect()
    }

    pub(crate) fn retain(&self) -> CommandLease {
        self.0.send_modify(|state| state.pending += 1);

        CommandLease(self.clone())
    }

    pub(crate) fn admit(&self, admission: Arc<OwnedMutexGuard<()>>) {
        self.0
            .send_modify(|state| state.admission = Some(admission));
    }

    pub(crate) fn register(
        &self,
        control: Arc<dyn ExecutionControl>,
        journal: Option<WriteJournal>,
    ) -> Arc<Operation> {
        let operation = Arc::new(Operation {
            control,
            executing: AtomicBool::new(false),
            id: Mutex::new(None),
            journal,
            outcome: Mutex::new(None),
            persisted: AtomicBool::new(true),
            reconciled: AtomicBool::new(false),
        });
        self.0
            .send_modify(|state| state.operations.push(Arc::clone(&operation)));

        operation
    }

    pub(crate) async fn settled(&self) -> Result<(), CommandSettlementError> {
        let mut receiver = self.0.subscribe();
        loop {
            {
                let state = receiver.borrow_and_update();
                if state.pending == 0 {
                    return if state.operations.iter().all(|operation| operation.settled()) {
                        Ok(())
                    } else {
                        Err(CommandSettlementError)
                    };
                }
            }
            let _ = receiver.changed().await;
        }
    }

    pub(crate) async fn retry(&self) -> Result<(), CommandSettlementError> {
        let operations = {
            let state = self.0.borrow();
            if state.pending != 0 {
                return Err(CommandSettlementError);
            }
            state.operations.clone()
        };
        for operation in operations {
            if operation.settled() {
                continue;
            }
            operation
                .control
                .cleanup()
                .await
                .map_err(|_: ExecutionError| CommandSettlementError)?;
            operation
                .persist()
                .await
                .map_err(|_| CommandSettlementError)?;
            let cleanup_failed = operation
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .is_none_or(|outcome| outcome.cleanup_failed);
            if cleanup_failed {
                let id = *operation
                    .id
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let (Some(journal), Some(id)) = (&operation.journal, id) {
                    journal
                        .reconcile_command(id)
                        .await
                        .map_err(|_| CommandSettlementError)?;
                }
                operation.reconciled.store(true, Ordering::Release);
            }
        }
        self.release();

        Ok(())
    }

    pub(crate) fn reconcile(record: &CommandRecord) {
        let retained = retained()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for commands in retained {
            let operations = commands.0.borrow().operations.clone();
            for operation in operations {
                let id = *operation
                    .id
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if id == Some(record.id)
                    && operation
                        .journal
                        .as_ref()
                        .is_some_and(|journal| journal.owner() == record.owner())
                {
                    operation.control.cancel();
                    operation.reconciled.store(true, Ordering::Release);
                }
            }
            commands.release();
        }
    }

    fn release(&self) {
        self.0.send_modify(|state| {
            if state.pending == 0 && state.operations.iter().all(|operation| operation.settled()) {
                state.admission = None;
            }
        });
        retained()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(Commands::unresolved);
    }

    fn unresolved(&self) -> bool {
        let state = self.0.borrow();

        state.pending != 0
            || state
                .operations
                .iter()
                .any(|operation| !operation.settled())
    }
}

#[derive(Default)]
struct State {
    admission: Option<Arc<OwnedMutexGuard<()>>>,
    operations: Vec<Arc<Operation>>,
    pending: usize,
}

pub(crate) struct CommandLease(Commands);

impl Drop for CommandLease {
    fn drop(&mut self) {
        let mut unresolved = false;
        self.0.0.send_modify(|state| {
            state.pending -= 1;
            if state.pending == 0 {
                unresolved = state
                    .operations
                    .iter()
                    .any(|operation| !operation.settled());
                if !unresolved {
                    state.admission = None;
                }
            }
        });
        if unresolved {
            let mut retained = retained()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            retained.retain(Commands::unresolved);
            if self.0.unresolved() {
                retained.push(self.0.clone());
            }
        }
    }
}

fn retained() -> &'static Mutex<Vec<Commands>> {
    static RETAINED: OnceLock<Mutex<Vec<Commands>>> = OnceLock::new();

    RETAINED.get_or_init(Mutex::default)
}

pub(crate) struct Operation {
    pub(crate) control: Arc<dyn ExecutionControl>,
    executing: AtomicBool,
    id: Mutex<Option<i64>>,
    journal: Option<WriteJournal>,
    outcome: Mutex<Option<CommandOutcome>>,
    persisted: AtomicBool,
    reconciled: AtomicBool,
}

impl Operation {
    pub(crate) fn admitted(&self, id: Option<i64>) {
        self.executing.store(true, Ordering::Release);
        *self
            .id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = id;
        self.persisted.store(id.is_none(), Ordering::Release);
    }

    pub(crate) fn observed(&self, outcome: CommandOutcome) {
        self.executing.store(false, Ordering::Release);
        *self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
    }

    pub(crate) async fn persist(&self) -> Result<(), SessionError> {
        let id = *self
            .id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let outcome = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let (Some(journal), Some(id), Some(outcome)) = (&self.journal, id, outcome) {
            journal.finish_command(id, &outcome).await?;
        }
        self.persisted.store(true, Ordering::Release);

        Ok(())
    }

    fn settled(&self) -> bool {
        self.reconciled.load(Ordering::Acquire)
            || (self.persisted.load(Ordering::Acquire)
                && !self.executing.load(Ordering::Acquire)
                && self
                    .outcome
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .is_none_or(|outcome| !outcome.cleanup_failed))
    }
}

#[cfg(test)]
#[path = "command_settlement_test.rs"]
mod tests;
