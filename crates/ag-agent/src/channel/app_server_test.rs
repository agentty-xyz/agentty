//! Unit tests for the app-server agent channel, split by behavior.

#[path = "app_server_test/repair_test.rs"]
mod repair;
#[path = "app_server_test/stream_test.rs"]
mod stream;
#[path = "app_server_test/support_test.rs"]
mod support;
#[path = "app_server_test/turn_test.rs"]
mod turn;
