use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, MutexGuard};

use tempfile::tempdir;

use super::*;

#[path = "availability_test/compatibility_test.rs"]
mod compatibility;
#[path = "availability_test/discovery_test.rs"]
mod discovery;
#[path = "availability_test/refresh_test.rs"]
mod refresh;
#[path = "availability_test/support_test.rs"]
mod support;

use support::*;
