//! Top-level tab definitions and state management.

/// Available top-level tabs in list mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tab {
    Projects,
    Sessions,
    Stats,
    Settings,
}

impl Tab {
    /// Returns the display label used in the tabs header.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Projects => "Projects",
            Tab::Sessions => "Sessions",
            Tab::Stats => "Stats",
            Tab::Settings => "Settings",
        }
    }

    /// Cycles to the next tab in display order.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Tab::Projects => Tab::Sessions,
            Tab::Sessions => Tab::Stats,
            Tab::Stats => Tab::Settings,
            Tab::Settings => Tab::Projects,
        }
    }

    /// Cycles to the previous tab in display order.
    #[must_use]
    pub fn previous(self) -> Self {
        match self {
            Tab::Projects => Tab::Settings,
            Tab::Sessions => Tab::Projects,
            Tab::Stats => Tab::Sessions,
            Tab::Settings => Tab::Stats,
        }
    }
}

/// Manages selection state for top-level tabs.
pub struct TabManager {
    current: Tab,
}

impl TabManager {
    /// Creates a manager with `Tab::Projects` selected.
    pub fn new() -> Self {
        Self {
            current: Tab::Projects,
        }
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

impl Default for TabManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tab_title() {
        // Arrange

        // Act
        let project_title = Tab::Projects.title();
        let session_title = Tab::Sessions.title();
        let stats_title = Tab::Stats.title();
        let settings_title = Tab::Settings.title();

        // Assert
        assert_eq!(project_title, "Projects");
        assert_eq!(session_title, "Sessions");
        assert_eq!(stats_title, "Stats");
        assert_eq!(settings_title, "Settings");
    }

    #[test]
    fn test_tab_next() {
        // Arrange

        // Act
        let project_next = Tab::Projects.next();
        let session_next = Tab::Sessions.next();
        let stats_next = Tab::Stats.next();
        let settings_next = Tab::Settings.next();

        // Assert
        assert_eq!(project_next, Tab::Sessions);
        assert_eq!(session_next, Tab::Stats);
        assert_eq!(stats_next, Tab::Settings);
        assert_eq!(settings_next, Tab::Projects);
    }

    #[test]
    fn test_tab_previous() {
        // Arrange

        // Act
        let project_previous = Tab::Projects.previous();
        let session_previous = Tab::Sessions.previous();
        let stats_previous = Tab::Stats.previous();
        let settings_previous = Tab::Settings.previous();

        // Assert
        assert_eq!(project_previous, Tab::Settings);
        assert_eq!(session_previous, Tab::Projects);
        assert_eq!(stats_previous, Tab::Sessions);
        assert_eq!(settings_previous, Tab::Stats);
    }

    #[test]
    fn test_tab_manager_new_defaults_to_projects() {
        // Arrange

        // Act
        let manager = TabManager::new();

        // Assert
        assert_eq!(manager.current(), Tab::Projects);
    }

    #[test]
    fn test_tab_manager_next_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();
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
                Tab::Stats,
                Tab::Settings,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_previous_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();
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
                Tab::Stats,
                Tab::Sessions,
                Tab::Projects
            ]
        );
    }

    #[test]
    fn test_tab_manager_set_updates_current_tab() {
        // Arrange
        let mut manager = TabManager::new();

        // Act
        manager.set(Tab::Settings);

        // Assert
        assert_eq!(manager.current(), Tab::Settings);
    }
}
