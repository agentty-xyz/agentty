//! List-view tab definitions and state management.

/// Describes whether a tab is global or tied to the active project.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabScope {
    Global,
    Project,
}

/// Available top-level tabs in list mode.
///
/// The derived `Default` selects `Tab::Projects` so newly initialized
/// navigation state starts on the projects tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Tab {
    #[default]
    Projects,
    Sessions,
    Review,
    Settings,
}

impl Tab {
    /// Tabs in the order they are rendered.
    pub const ALL: [Self; 4] = [Self::Projects, Self::Sessions, Self::Review, Self::Settings];
    /// Project-scoped tabs in display order.
    pub const PROJECT_SCOPED: [Self; 3] = [Self::Sessions, Self::Review, Self::Settings];

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
            Tab::Review => "Review",
            Tab::Settings => "Settings",
        }
    }

    /// Returns whether the tab is global or tied to the active project.
    #[must_use]
    pub fn scope(self) -> TabScope {
        match self {
            Tab::Projects => TabScope::Global,
            Tab::Sessions | Tab::Review | Tab::Settings => TabScope::Project,
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
mod tests {
    use super::*;

    #[test]
    fn test_tab_title() {
        // Arrange

        // Act
        let titles = Tab::ALL.map(Tab::title);

        // Assert
        assert_eq!(titles, ["Projects", "Sessions", "Review", "Settings"]);
    }

    #[test]
    fn test_tab_scope_marks_only_projects_as_global() {
        // Arrange

        // Act
        let scopes = Tab::ALL.map(Tab::scope);

        // Assert
        assert_eq!(
            scopes,
            [
                TabScope::Global,
                TabScope::Project,
                TabScope::Project,
                TabScope::Project
            ]
        );
    }

    #[test]
    fn test_tab_next_cycles_in_display_order() {
        // Arrange

        // Act
        let next_tabs = Tab::ALL.map(Tab::next);

        // Assert
        assert_eq!(
            next_tabs,
            [Tab::Sessions, Tab::Review, Tab::Settings, Tab::Projects]
        );
    }

    #[test]
    fn test_tab_previous_cycles_in_display_order() {
        // Arrange

        // Act
        let previous_tabs = Tab::ALL.map(Tab::previous);

        // Assert
        assert_eq!(
            previous_tabs,
            [Tab::Settings, Tab::Projects, Tab::Sessions, Tab::Review]
        );
    }

    #[test]
    fn test_tab_project_scoped_order_keeps_project_pages_grouped() {
        // Arrange

        // Act
        let project_scoped_tabs = Tab::project_scoped_tabs();

        // Assert
        assert_eq!(
            project_scoped_tabs,
            &[Tab::Sessions, Tab::Review, Tab::Settings]
        );
    }

    #[test]
    fn test_tab_manager_new_defaults_to_projects() {
        // Arrange

        // Act
        let manager = TabManager::default();

        // Assert
        assert_eq!(manager.current(), Tab::Projects);
    }

    #[test]
    fn test_tab_manager_next_cycles_tabs_with_tasks() {
        // Arrange
        let mut manager = TabManager::default();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());
        manager.next();
        observed_tabs.push(manager.current());

        // Assert
        assert_eq!(
            observed_tabs,
            vec![
                Tab::Projects,
                Tab::Sessions,
                Tab::Review,
                Tab::Settings,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_previous_cycles_tabs_without_tasks() {
        // Arrange
        let mut manager = TabManager::default();
        let mut observed_tabs = Vec::new();

        // Act
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());
        manager.previous();
        observed_tabs.push(manager.current());

        // Assert
        assert_eq!(
            observed_tabs,
            vec![
                Tab::Projects,
                Tab::Settings,
                Tab::Review,
                Tab::Sessions,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_set_updates_current_tab() {
        // Arrange
        let mut manager = TabManager::default();

        // Act
        manager.set(Tab::Settings);

        // Assert
        assert_eq!(manager.current(), Tab::Settings);
    }
}
