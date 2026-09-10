#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use super::*;

#[path = "prompt_test/attachment_test.rs"]
mod attachment;
#[path = "prompt_test/format_test.rs"]
mod format;
#[path = "prompt_test/instruction_test.rs"]
mod instruction;
#[path = "prompt_test/support_test.rs"]
mod support;

use support::*;
