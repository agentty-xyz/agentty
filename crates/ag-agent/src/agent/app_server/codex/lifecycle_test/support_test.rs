use super::*;

/// Captures the dynamic JSON-RPC `id` from a written payload through the
/// supplied mutex so the response side of a mock can echo it back.
pub(super) fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Ok(mut guard) = id_store.lock() {
        *guard = id;
    }
}
