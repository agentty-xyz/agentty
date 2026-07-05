use std::env;

use super::{ClipboardBackend, wayland, x11};
use crate::ClipboardError;

pub(crate) fn new_backend() -> Result<Box<dyn ClipboardBackend>, ClipboardError> {
    match select_backend(&LinuxClipboardEnvironment::from_env()) {
        LinuxBackendSelection::Wayland => Ok(Box::new(wayland::WaylandClipboard::new()?)),
        LinuxBackendSelection::X11 => Ok(Box::new(x11::X11Clipboard::new()?)),
        LinuxBackendSelection::Unavailable => Err(ClipboardError::Unavailable {
            reason: "no supported Linux clipboard display was detected".to_string(),
        }),
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct LinuxClipboardEnvironment {
    display: Option<String>,
    wayland_display: Option<String>,
}

impl LinuxClipboardEnvironment {
    fn from_env() -> Self {
        Self {
            display: env::var("DISPLAY").ok(),
            wayland_display: env::var("WAYLAND_DISPLAY").ok(),
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum LinuxBackendSelection {
    Unavailable,
    Wayland,
    X11,
}

fn select_backend(environment: &LinuxClipboardEnvironment) -> LinuxBackendSelection {
    if environment
        .wayland_display
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return LinuxBackendSelection::Wayland;
    }

    if environment
        .display
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        return LinuxBackendSelection::X11;
    }

    LinuxBackendSelection::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_select_backend_prefers_wayland_over_x11() {
        // Arrange
        let environment = LinuxClipboardEnvironment {
            display: Some(":0".to_string()),
            wayland_display: Some("wayland-0".to_string()),
        };

        // Act
        let backend_selection = select_backend(&environment);

        // Assert
        assert_eq!(backend_selection, LinuxBackendSelection::Wayland);
    }

    #[test]
    fn test_select_backend_uses_x11_when_only_display_is_set() {
        // Arrange
        let environment = LinuxClipboardEnvironment {
            display: Some(":0".to_string()),
            wayland_display: None,
        };

        // Act
        let backend_selection = select_backend(&environment);

        // Assert
        assert_eq!(backend_selection, LinuxBackendSelection::X11);
    }

    #[test]
    fn test_select_backend_reports_unavailable_without_display_variables() {
        // Arrange
        let environment = LinuxClipboardEnvironment {
            display: None,
            wayland_display: None,
        };

        // Act
        let backend_selection = select_backend(&environment);

        // Assert
        assert_eq!(backend_selection, LinuxBackendSelection::Unavailable);
    }
}
