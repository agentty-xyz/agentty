//! In-memory registry of instances and the streams waiting for them.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, oneshot};

use crate::close::CloseReason;
use crate::limit::RelayLimits;

/// Registered instances and unanswered streams.
///
/// `Socket` is what an instance hands over to answer a stream: the relay
/// uses a WebSocket, and tests use plain values.
pub(crate) struct Registry<Socket> {
    inner: Mutex<Inner<Socket>>,
}

impl<Socket> Registry<Socket> {
    /// Creates an empty registry.
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                instances: HashMap::new(),
                next_generation: 0,
                pending: HashMap::new(),
                stream_counts: HashMap::new(),
            }),
        }
    }

    /// Registers `instance_id` for a control connection from `address`.
    ///
    /// A registration of an identifier that is already registered replaces
    /// the earlier one, whose stream receiver then ends. That is safe because
    /// the caller has verified the instance owns the identifier's key.
    pub(crate) fn register(
        &self,
        instance_id: &str,
        address: IpAddr,
        limits: &RelayLimits,
    ) -> Result<Registration, CloseReason> {
        let mut inner = self.lock();
        let is_replacement = inner.instances.contains_key(instance_id);
        if !is_replacement && inner.instances.len() >= limits.max_instances {
            return Err(CloseReason::RelayFull);
        }
        let others_from_address = inner
            .instances
            .iter()
            .filter(|(id, instance)| instance.address == address && id.as_str() != instance_id)
            .count();
        if others_from_address >= limits.max_instances_per_address {
            return Err(CloseReason::TooManyInstances);
        }
        let (streams_tx, streams) = mpsc::channel(limits.max_streams_per_instance.max(1));
        inner.next_generation += 1;
        let generation = inner.next_generation;
        inner.instances.insert(
            instance_id.to_string(),
            Instance {
                address,
                generation,
                streams_tx,
            },
        );

        Ok(Registration {
            generation,
            streams,
        })
    }

    /// Removes the registration made as `generation`; a newer registration of
    /// the same identifier stays.
    pub(crate) fn unregister(&self, instance_id: &str, generation: u64) {
        let mut inner = self.lock();
        let is_current = inner
            .instances
            .get(instance_id)
            .is_some_and(|instance| instance.generation == generation);
        if is_current {
            inner.instances.remove(instance_id);
        }
    }

    /// Whether an instance is registered under `instance_id`.
    pub(crate) fn has_instance(&self, instance_id: &str) -> bool {
        self.lock().instances.contains_key(instance_id)
    }

    /// Opens `stream_id` toward `instance_id` and tells the instance about
    /// it. The returned receiver yields what the instance answers with.
    ///
    /// The stream counts against the instance until
    /// [`Registry::close_stream`].
    pub(crate) fn open_stream(
        &self,
        instance_id: &str,
        stream_id: &str,
        limits: &RelayLimits,
    ) -> Result<oneshot::Receiver<Socket>, CloseReason> {
        let mut inner = self.lock();
        let Some(instance) = inner.instances.get(instance_id) else {
            return Err(CloseReason::UnknownInstance);
        };
        let streams_tx = instance.streams_tx.clone();
        let open_streams = inner.stream_counts.get(instance_id).copied().unwrap_or(0);
        if open_streams >= limits.max_streams_per_instance {
            return Err(CloseReason::TooManyStreams);
        }
        if streams_tx.try_send(stream_id.to_string()).is_err() {
            return Err(CloseReason::InstanceBusy);
        }
        let (answer_tx, answer_rx) = oneshot::channel();
        inner.pending.insert(stream_id.to_string(), answer_tx);
        *inner
            .stream_counts
            .entry(instance_id.to_string())
            .or_default() += 1;

        Ok(answer_rx)
    }

    /// Whether `stream_id` still waits for its instance.
    pub(crate) fn has_pending(&self, stream_id: &str) -> bool {
        self.lock().pending.contains_key(stream_id)
    }

    /// Takes the sender that answers `stream_id`; `None` when the stream is
    /// unknown, already answered, or closed.
    pub(crate) fn accept_stream(&self, stream_id: &str) -> Option<oneshot::Sender<Socket>> {
        self.lock().pending.remove(stream_id)
    }

    /// Ends a stream opened with [`Registry::open_stream`].
    pub(crate) fn close_stream(&self, instance_id: &str, stream_id: &str) {
        let mut inner = self.lock();
        inner.pending.remove(stream_id);
        let remaining = inner
            .stream_counts
            .get(instance_id)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if remaining == 0 {
            inner.stream_counts.remove(instance_id);
        } else {
            inner
                .stream_counts
                .insert(instance_id.to_string(), remaining);
        }
    }

    /// Locks the registry; a poisoned lock still holds consistent maps.
    fn lock(&self) -> MutexGuard<'_, Inner<Socket>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Outcome of a successful registration.
pub(crate) struct Registration {
    /// Identifies this registration when it unregisters.
    pub(crate) generation: u64,
    /// Yields the identifier of each stream opened toward the instance, and
    /// ends when a newer registration replaces this one.
    pub(crate) streams: mpsc::Receiver<String>,
}

/// Maps guarded by the registry lock.
struct Inner<Socket> {
    /// Registered instances by identifier.
    instances: HashMap<String, Instance>,
    /// Generation given to the latest registration.
    next_generation: u64,
    /// Answer senders of unanswered streams by stream identifier.
    pending: HashMap<String, oneshot::Sender<Socket>>,
    /// Open and unanswered streams by instance identifier.
    stream_counts: HashMap<String, usize>,
}

/// One registered instance.
struct Instance {
    /// Client address the control connection came from.
    address: IpAddr,
    /// Registration that owns this entry.
    generation: u64,
    /// Delivers stream identifiers to the control connection.
    streams_tx: mpsc::Sender<String>,
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
