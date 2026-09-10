use super::{Tab, TabManager, TabScope};

#[test]
fn test_tab_title() {
    // Arrange

    // Act
    let titles = Tab::ALL.map(Tab::title);

    // Assert
    assert_eq!(titles, ["Projects", "Sessions", "Settings"]);
}

#[test]
fn test_tab_scope_marks_only_projects_as_global() {
    // Arrange

    // Act
    let scopes = Tab::ALL.map(Tab::scope);

    // Assert
    assert_eq!(
        scopes,
        [TabScope::Global, TabScope::Project, TabScope::Project]
    );
}

#[test]
fn test_tab_from_str_parses_persisted_values() {
    // Arrange
    let values = [
        ("Projects", Some(Tab::Projects)),
        ("Sessions", Some(Tab::Sessions)),
        ("Settings", Some(Tab::Settings)),
        ("Invalid", None),
    ];

    // Act & Assert
    for (value, expected_tab) in values {
        assert_eq!(Tab::from_str(value), expected_tab);
    }
}

#[test]
fn test_tab_as_str_matches_persisted_values() {
    // Arrange

    // Act
    let values = Tab::ALL.map(Tab::as_str);

    // Assert
    assert_eq!(values, ["Projects", "Sessions", "Settings"]);
}

#[test]
fn test_tab_next_cycles_in_display_order() {
    // Arrange

    // Act
    let next_tabs = Tab::ALL.map(Tab::next);

    // Assert
    assert_eq!(next_tabs, [Tab::Sessions, Tab::Settings, Tab::Projects]);
}

#[test]
fn test_tab_previous_cycles_in_display_order() {
    // Arrange

    // Act
    let previous_tabs = Tab::ALL.map(Tab::previous);

    // Assert
    assert_eq!(previous_tabs, [Tab::Settings, Tab::Projects, Tab::Sessions]);
}

#[test]
fn test_tab_project_scoped_order_keeps_project_pages_grouped() {
    // Arrange

    // Act
    let project_scoped_tabs = Tab::project_scoped_tabs();

    // Assert
    assert_eq!(project_scoped_tabs, &[Tab::Sessions, Tab::Settings]);
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
fn test_tab_manager_new_uses_explicit_tab() {
    // Arrange

    // Act
    let manager = TabManager::new(Tab::Sessions);

    // Assert
    assert_eq!(manager.current(), Tab::Sessions);
}

#[test]
fn test_tab_manager_next_cycles_all_tabs() {
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

    // Assert
    assert_eq!(
        observed_tabs,
        vec![Tab::Projects, Tab::Sessions, Tab::Settings, Tab::Projects]
    );
}

#[test]
fn test_tab_manager_previous_cycles_all_tabs() {
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

    // Assert
    assert_eq!(
        observed_tabs,
        vec![Tab::Projects, Tab::Settings, Tab::Sessions, Tab::Projects]
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
