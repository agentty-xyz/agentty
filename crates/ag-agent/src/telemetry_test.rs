use tokio::sync::Mutex;

/// Holds exclusive provider ownership through execution and span assertions.
/// Every test that replaces the global tracer provider must acquire this lock.
pub(crate) static TRACER_PROVIDER_LOCK: Mutex<()> = Mutex::const_new(());
