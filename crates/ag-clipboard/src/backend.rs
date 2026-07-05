mod contract;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported;
#[cfg(any(target_os = "linux", test))]
mod wayland;
#[cfg(target_os = "linux")]
mod x11;

pub(crate) use contract::ClipboardBackend;
#[cfg(target_os = "linux")]
pub(crate) use linux::new_backend;
#[cfg(target_os = "macos")]
pub(crate) use macos::new_backend;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) use unsupported::new_backend;
