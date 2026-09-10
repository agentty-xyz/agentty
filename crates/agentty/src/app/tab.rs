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
}

impl Tab {
    /// Tabs in the order they are rendered.
    pub const ALL: [Self; 3] = [Self::Projects, Self::Sessions, Self::Settings];
    /// Project-scoped tabs in display order.
    pub const PROJECT_SCOPED: [Self; 2] = [Self::Sessions, Self::Settings];

    /// Returns the available top-level tabs.
    pub fn available_tabs() -> &'static [Self] {
        &Self::ALL
    }

    /// Returns the project-scoped tabs available for the current project.
    pub fn project_scoped_tabs() -> &'static [Self] {
        &Self::PROJECT_SCOPED
    }

    /// Returns the display label used in the tabs header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Settings => "Settings",
        }
    }

    /// Returns the stable persisted value used for startup restoration.
    pub(crate) fn as_str(self) -> &'static str {
        self.title()
    }

    /// Parses one persisted tab value.
    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "Projects" => Some(Self::Projects),
            "Sessions" => Some(Self::Sessions),
            "Settings" => Some(Self::Settings),
            _ => None,
        }
    }

    /// Returns whether the tab is global or tied to the active project.
    #[must_use]
    pub fn scope(self) -> TabScope {
        match self {
            Tab::Projects => TabScope::Global,
            Tab::Sessions | Tab::Settings => TabScope::Project,
        }
    }

    /// Cycles to the next tab in display order.
    #[must_use]
    fn next(self) -> Self {
        let tabs = Self::available_tabs();
        let tab_index = self.index();
        let next_index = (tab_index + 1) % tabs.len();

        tabs[next_index]
    }

    /// Cycles to the previous tab in display order.
    #[must_use]
    fn previous(self) -> Self {
        let tabs = Self::available_tabs();
        let tab_index = self.index();
        let previous_index = (tab_index + tabs.len() - 1) % tabs.len();

        tabs[previous_index]
    }

    /// Returns the display-order index for the tab.
    fn index(self) -> usize {
        match Self::available_tabs().iter().position(|tab| *tab == self) {
            Some(tab_index) => tab_index,
            None => unreachable!("tab must exist in the display order"),
        }
    }
}

/// Manages selection state for top-level tabs.
///
/// The derived `Default` initializes `current` to `Tab::default()`, which
/// selects `Tab::Projects` on a freshly constructed manager.
#[derive(Default)]
pub struct TabManager {
    current: Tab,
}

impl TabManager {
    /// Builds a manager with an explicit starting tab.
    #[must_use]
    pub fn new(current: Tab) -> Self {
        Self { current }
    }

    /// Returns the currently selected tab.
    #[must_use]
    pub fn current(&self) -> Tab {
        self.current
    }

    /// Cycles selection to the next tab.
    pub fn next(&mut self) {
        self.current = self.current.next();
    }

    /// Cycles selection to the previous tab.
    pub fn previous(&mut self) {
        self.current = self.current.previous();
    }

    /// Sets the currently selected tab.
    pub fn set(&mut self, tab: Tab) {
        self.current = tab;
    }
}

#[cfg(test)]
#[path = "tab_test.rs"]
mod tests;
