//! Explicit lifecycle state for non-durable session-output messages.

use std::hash::{Hash, Hasher};

use rustc_hash::FxHasher;

/// Stable identity for one replaceable session-output message.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum TransientMessageSlot {
    /// Published session summary for the latest completed turn.
    Summary,
    /// Focused review loading, result, or failure output.
    Review,
    /// Short-lived workflow feedback produced while finalizing a turn.
    WorkflowNotice,
    /// Manual branch or review-request publish progress and result.
    BranchPublish,
    /// Published-branch auto-push progress replaced by its durable result.
    PublishedBranchSync,
}

/// Placement of one transient message relative to durable transcript content.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TransientMessageAnchor {
    /// Immediately after durable content from the latest completed turn.
    AfterCompletedTurn,
    /// At the end of the output, beside other active status rows.
    Tail,
}

/// Removal policy for one transient message.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TransientMessageLifecycle {
    /// Remove when a later user turn becomes active.
    ClearOnNewTurn,
    /// Retain until its owning workflow explicitly resolves it.
    UntilResolved,
}

/// Typed content for one transient message.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TransientMessageBody {
    /// Markdown content rendered through the shared markdown cache.
    Markdown(String),
    /// Plain status or failure text that must not interpret markdown syntax.
    Plain(String),
    /// Animated status text with explicit loading semantics.
    Loading(String),
}

impl TransientMessageBody {
    /// Returns the message text independent of its render treatment.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Markdown(text) | Self::Plain(text) | Self::Loading(text) => text,
        }
    }
}

/// One replaceable session-output message with explicit placement and lifetime.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct TransientMessage {
    pub(crate) anchor: TransientMessageAnchor,
    pub(crate) body: TransientMessageBody,
    pub(crate) lifecycle: TransientMessageLifecycle,
    pub(crate) slot: TransientMessageSlot,
    /// User-prompt transcript position that produced this message, when known.
    pub(crate) turn_position: Option<i64>,
}

/// Per-session slot store for non-durable output messages.
///
/// `version` changes only when observable slot content changes. `fingerprint`
/// gives render caches a content identity that remains valid when refreshes
/// reconstruct an equivalent store with a new local version sequence. Slots
/// use canonical enum order rather than async event arrival order, so status
/// replacement does not move surrounding output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TransientMessageStore {
    fingerprint: u64,
    messages: Vec<TransientMessage>,
    version: u64,
}

impl TransientMessageStore {
    /// Returns the monotonic observable-content version.
    pub(crate) fn version(&self) -> u64 {
        self.version
    }

    /// Returns a content-derived cache identity that remains stable when
    /// equivalent messages are reconstructed in a refreshed session snapshot.
    pub(crate) fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Returns all current messages in stable display order.
    pub(crate) fn messages(&self) -> &[TransientMessage] {
        &self.messages
    }

    /// Returns one current slot value.
    pub(crate) fn get(&self, slot: TransientMessageSlot) -> Option<&TransientMessage> {
        self.messages.iter().find(|message| message.slot == slot)
    }

    /// Posts a new slot or replaces its content without changing its position.
    pub(crate) fn upsert(&mut self, message: TransientMessage) {
        if let Some(existing) = self
            .messages
            .iter_mut()
            .find(|existing| existing.slot == message.slot)
        {
            if *existing == message {
                return;
            }

            *existing = message;
            self.bump_version();

            return;
        }

        self.messages.push(message);
        self.messages.sort_by_key(|message| message.slot);
        self.bump_version();
    }

    /// Removes one slot and returns its previous value.
    pub(crate) fn retract(&mut self, slot: TransientMessageSlot) -> Option<TransientMessage> {
        let message_index = self
            .messages
            .iter()
            .position(|message| message.slot == slot)?;
        let message = self.messages.remove(message_index);
        self.bump_version();

        Some(message)
    }

    /// Removes turn-scoped messages with no producing turn or from before
    /// `active_turn_position`.
    pub(crate) fn clear_for_new_turn(&mut self, active_turn_position: i64) {
        let previous_len = self.messages.len();
        self.messages.retain(|message| {
            message.lifecycle != TransientMessageLifecycle::ClearOnNewTurn
                || message
                    .turn_position
                    .is_some_and(|turn_position| turn_position >= active_turn_position)
        });
        if self.messages.len() != previous_len {
            self.bump_version();
        }
    }

    fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
        self.fingerprint = Self::calculate_fingerprint(&self.messages);
    }

    fn calculate_fingerprint(messages: &[TransientMessage]) -> u64 {
        let mut hasher = FxHasher::default();
        messages.hash(&mut hasher);

        hasher.finish()
    }
}

impl Default for TransientMessageStore {
    fn default() -> Self {
        let messages = Vec::new();
        let fingerprint = Self::calculate_fingerprint(&messages);

        Self {
            fingerprint,
            messages,
            version: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(slot: TransientMessageSlot, text: &str, turn_position: i64) -> TransientMessage {
        TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown(text.to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot,
            turn_position: Some(turn_position),
        }
    }

    #[test]
    fn upsert_replaces_slot_without_moving_it() {
        // Arrange
        let mut store = TransientMessageStore::default();
        store.upsert(message(TransientMessageSlot::Summary, "summary", 1));
        store.upsert(message(TransientMessageSlot::Review, "review", 1));

        // Act
        store.upsert(message(TransientMessageSlot::Summary, "new summary", 1));

        // Assert
        assert_eq!(store.messages[0].body.text(), "new summary");
        assert_eq!(store.messages[1].slot, TransientMessageSlot::Review);
        assert_eq!(store.version(), 3);
    }

    #[test]
    fn clear_for_new_turn_only_retracts_older_turn_scoped_messages() {
        // Arrange
        let mut store = TransientMessageStore::default();
        store.upsert(message(TransientMessageSlot::Summary, "old", 2));
        store.upsert(message(TransientMessageSlot::Review, "current", 3));
        store.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Markdown("unbound".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::WorkflowNotice,
            turn_position: None,
        });
        store.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            body: TransientMessageBody::Loading("Pushing...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::BranchPublish,
            turn_position: Some(2),
        });

        // Act
        store.clear_for_new_turn(3);

        // Assert
        assert!(store.get(TransientMessageSlot::Summary).is_none());
        assert!(store.get(TransientMessageSlot::Review).is_some());
        assert!(store.get(TransientMessageSlot::WorkflowNotice).is_none());
        assert!(store.get(TransientMessageSlot::BranchPublish).is_some());
    }

    #[test]
    fn fingerprint_distinguishes_reconstructed_stores_with_matching_versions() {
        // Arrange
        let mut loading_store = TransientMessageStore::default();
        loading_store.upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Reviewing changes".to_string()),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: Some(1),
        });
        let mut ready_store = TransientMessageStore::default();
        ready_store.upsert(message(
            TransientMessageSlot::Review,
            "## Review\nFinding",
            1,
        ));

        // Act
        let loading_fingerprint = loading_store.fingerprint();
        let ready_fingerprint = ready_store.fingerprint();

        // Assert
        assert_eq!(loading_store.version(), ready_store.version());
        assert_ne!(loading_fingerprint, ready_fingerprint);
    }

    #[test]
    fn fingerprint_matches_fresh_store_after_last_message_is_retracted() {
        // Arrange
        let fresh_store = TransientMessageStore::default();
        let mut emptied_store = TransientMessageStore::default();
        emptied_store.upsert(message(TransientMessageSlot::Review, "review", 1));

        // Act
        let retracted_message = emptied_store.retract(TransientMessageSlot::Review);

        // Assert
        assert!(retracted_message.is_some());
        assert!(emptied_store.messages().is_empty());
        assert_eq!(emptied_store.fingerprint(), fresh_store.fingerprint());
    }
}
