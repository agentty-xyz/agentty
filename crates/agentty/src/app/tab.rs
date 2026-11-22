//! List-view tab definitions and state management.

/// Describes whether a tab is global or tied to the active project.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabScope {
    /// Tab is available without an active project.
    Global,
    /// Tab displays data belonging to the active project.
    Project,
}

/// Available top-level tabs in list mode.
///
/// The derived `Default` selects `Tab::Projects` so newly initialized
/// navigation state starts on the projects tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    /// Project selection and management.
    #[default]
    Projects,
    /// Sessions belonging to the active project.
    Sessions,
    /// Settings for the active project.
    Settings,
    /// Remote control of the active project's sessions, offered only with
    /// `--experimental-remote`.
    Remote,
}

impl Tab {
    /// Every tab in the order they are rendered.
    pub const ALL: [Self; 4] = [Self::Projects, Self::Sessions, Self::Settings, Self::Remote];
    /// Tabs offered while remote control is disabled, in display order.
    pub const WITHOUT_REMOTE: [Self; 3] = [Self::Projects, Self::Sessions, Self::Settings];

    /// Returns the tab's name: its persisted value and its header label,
    /// which the header extends for [`Tab::Remote`] while devices connect.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Settings => "Settings",
            Tab::Remote => "Remote",
        }
    }

    /// Returns the stable persisted value used for startup restoration.
    pub(crate) fn as_str(self) -> &'static str {
        self.title()
    }

    /// Parses one persisted tab value.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.as_str() == value)
    }

    /// Returns whether the tab is global or tied to the active project.
    #[must_use]
    pub fn scope(self) -> TabScope {
        match self {
            Tab::Projects => TabScope::Global,
            Tab::Sessions | Tab::Settings | Tab::Remote => TabScope::Project,
        }
    }
}

/// Manages selection state for top-level tabs.
///
/// The derived `Default` initializes `current` to `Tab::default()`, which
/// selects `Tab::Projects` on a freshly constructed manager, and leaves
/// [`Tab::Remote`] out of the cycle.
#[derive(Default)]
pub struct TabManager {
    current: Tab,
    /// Whether `--experimental-remote` offers [`Tab::Remote`].
    is_remote_available: bool,
}

impl TabManager {
    /// Builds a manager with an explicit starting tab and without
    /// [`Tab::Remote`].
    #[must_use]
    pub fn new(current: Tab) -> Self {
        Self {
            current,
            is_remote_available: false,
        }
    }

    /// Adds [`Tab::Remote`] to the tabs this launch offers.
    pub fn enable_remote(&mut self) {
        self.is_remote_available = true;
    }

    /// Returns the tabs this launch offers, in display order.
    #[must_use]
    pub fn available_tabs(&self) -> &'static [Tab] {
        if self.is_remote_available {
            &Tab::ALL
        } else {
            &Tab::WITHOUT_REMOTE
        }
    }

    /// Returns the currently selected tab. A selected tab this launch does
    /// not offer, such as [`Tab::Remote`] restored from a launch with
    /// `--experimental-remote`, reads as [`Tab::Sessions`].
    #[must_use]
    pub fn current(&self) -> Tab {
        if self.available_tabs().contains(&self.current) {
            self.current
        } else {
            Tab::Sessions
        }
    }

    /// Cycles selection to the next offered tab.
    pub fn next(&mut self) {
        self.current = self.offset_tab(1);
    }

    /// Cycles selection to the previous offered tab.
    pub fn previous(&mut self) {
        self.current = self.offset_tab(self.available_tabs().len() - 1);
    }

    /// Sets the currently selected tab.
    pub fn set(&mut self, tab: Tab) {
        self.current = tab;
    }

    /// Returns the offered tab `offset` places after the current one,
    /// wrapping around.
    fn offset_tab(&self, offset: usize) -> Tab {
        let tabs = self.available_tabs();
        let current = self.current();
        let current_index = tabs
            .iter()
            .position(|tab| *tab == current)
            .unwrap_or_default();

        tabs[(current_index + offset) % tabs.len()]
    }
}

#[cfg(test)]
#[path = "tab_test.rs"]
mod tests;
