//! Unit tests for durable SQLite sessions, split by behavior.

#[path = "sqlite_store_test/chat_test.rs"]
mod chat;
#[path = "sqlite_store_test/contention_test.rs"]
mod contention;
#[path = "sqlite_store_test/history_test.rs"]
mod history;
#[path = "sqlite_store_test/options_test.rs"]
mod options;
#[path = "sqlite_store_test/reservation_test.rs"]
mod reservation;
#[path = "sqlite_store_test/storage_test.rs"]
mod storage;
#[path = "sqlite_store_test/support_test.rs"]
mod support;
#[path = "write_journal_test.rs"]
mod write_journal;

#[path = "sqlite_store_test/store_test.rs"]
mod store;

#[path = "sqlite_store_test/recovery_test.rs"]
mod recovery;

#[path = "sqlite_store_test/model_switch_test.rs"]
mod model_switch;
