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
    /// Sign-in to an Agentty Platform and remote control through its relay.
    Platform,
}

impl Tab {
    /// Every tab in the order they are rendered.
    pub const ALL: [Self; 4] = [
        Self::Projects,
        Self::Sessions,
        Self::Settings,
        Self::Platform,
    ];

    /// Returns the tab's name: its persisted value and its header label,
    /// which the header extends for [`Tab::Platform`] while remote control
    /// runs.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Settings => "Settings",
            Tab::Platform => "Platform",
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
            Tab::Sessions | Tab::Settings | Tab::Platform => TabScope::Project,
        }
    }
}

/// Manages selection state for top-level tabs.
///
/// The derived `Default` initializes `current` to `Tab::default()`, which
/// selects `Tab::Projects` on a freshly constructed manager, and leaves
/// [`Tab::Platform`] out of the cycle.
#[derive(Default)]
pub struct TabManager {
    current: Tab,
    /// Whether this launch offers [`Tab::Platform`].
    is_platform_available: bool,
}

impl TabManager {
    /// Builds a manager with an explicit starting tab and without
    /// [`Tab::Platform`].
    #[must_use]
    pub fn new(current: Tab) -> Self {
        Self {
            current,
            is_platform_available: false,
        }
    }

    /// Adds [`Tab::Platform`] to the tabs this launch offers.
    pub fn enable_platform(&mut self) {
        self.is_platform_available = true;
    }

    /// Returns the tabs this launch offers, in display order.
    #[must_use]
    pub fn available_tabs(&self) -> Vec<Tab> {
        Tab::ALL
            .into_iter()
            .filter(|tab| match tab {
                Tab::Platform => self.is_platform_available,
                Tab::Projects | Tab::Sessions | Tab::Settings => true,
            })
            .collect()
    }

    /// Returns the currently selected tab. A selected tab this launch does
    /// not offer reads as [`Tab::Sessions`].
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
