use super::*;

/// Serializes tests that update the process-wide Antigravity compatibility
/// snapshot.
static ANTIGRAVITY_CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Acquires the test-only Antigravity cache guard, recovering after a
/// failed assertion poisoned an earlier guard.
pub(super) fn antigravity_cache_test_guard() -> MutexGuard<'static, ()> {
    ANTIGRAVITY_CACHE_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
