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
        // Arrange & Act & Assert
        assert_eq!(Tab::Projects.title(), "Projects");
        assert_eq!(Tab::Sessions.title(), "Sessions");
        assert_eq!(Tab::Stats.title(), "Stats");
        assert_eq!(Tab::Settings.title(), "Settings");
    }

    #[test]
    fn test_tab_next() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Projects.next(), Tab::Sessions);
        assert_eq!(Tab::Sessions.next(), Tab::Stats);
        assert_eq!(Tab::Stats.next(), Tab::Settings);
        assert_eq!(Tab::Settings.next(), Tab::Projects);
    }

    #[test]
    fn test_tab_previous() {
        // Arrange & Act & Assert
        assert_eq!(Tab::Projects.previous(), Tab::Settings);
        assert_eq!(Tab::Sessions.previous(), Tab::Projects);
        assert_eq!(Tab::Stats.previous(), Tab::Sessions);
        assert_eq!(Tab::Settings.previous(), Tab::Stats);
    }

    #[test]
    fn test_tab_manager_new_defaults_to_projects() {
        // Arrange & Act
        let manager = TabManager::new();

        // Assert
        assert_eq!(manager.current(), Tab::Projects);
    }

    #[test]
    fn test_tab_manager_next_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();

        // Act & Assert
        assert_eq!(manager.current(), Tab::Projects);
        manager.next();
        assert_eq!(manager.current(), Tab::Sessions);
        manager.next();
        assert_eq!(manager.current(), Tab::Stats);
        manager.next();
        assert_eq!(manager.current(), Tab::Settings);
        manager.next();
        assert_eq!(manager.current(), Tab::Projects);
    }

    #[test]
    fn test_tab_manager_previous_cycles_tabs() {
        // Arrange
        let mut manager = TabManager::new();

        // Act & Assert
        assert_eq!(manager.current(), Tab::Projects);
        manager.previous();
        assert_eq!(manager.current(), Tab::Settings);
        manager.previous();
        assert_eq!(manager.current(), Tab::Stats);
        manager.previous();
        assert_eq!(manager.current(), Tab::Sessions);
        manager.previous();
        assert_eq!(manager.current(), Tab::Projects);
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
