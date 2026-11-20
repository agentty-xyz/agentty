//! Bash command execution: shared contracts, the supervisor, and sandboxed
//! and unsandboxed executors. The native sandboxed executor and its launcher
//! require the `native-sandbox` feature.

mod contract;
#[cfg(all(feature = "native-sandbox", any(target_os = "linux", test)))]
mod landlock;
#[cfg(feature = "native-sandbox")]
mod launcher;
#[cfg(feature = "native-sandbox")]
mod native;
mod output;
#[cfg(all(feature = "native-sandbox", any(target_os = "linux", test)))]
mod seccomp;
mod supervisor;
mod system;
mod tool;
mod unsandboxed;
#[cfg(feature = "native-sandbox")]
mod wire;

pub(crate) use contract::ExecutionControl;
pub use contract::{
    BashExecutor, BashProcess, Enforcement, ExecutionAccess, ExecutionCommand, ExecutionError,
    ExecutionPolicy, MainExit, OutputStream, ProcessEvent,
};
#[cfg(feature = "native-sandbox")]
pub(crate) use launcher::{is_helper, run as run_launcher};
#[cfg(feature = "native-sandbox")]
pub(crate) use native::{Host, NativeLaunch, probe};
pub(crate) use tool::BashTool;
pub use unsandboxed::UnsandboxedExecutor;
