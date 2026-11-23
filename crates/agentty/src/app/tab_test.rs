//! List-view tab parsing, ordering, and `TabManager` cycling tests.

use super::{Tab, TabManager, TabScope};

#[test]
fn test_tab_title() {
    // Arrange

    // Act
    let titles = Tab::ALL.map(Tab::title);

    // Assert
    assert_eq!(
        titles,
        ["Projects", "Sessions", "Settings", "Remote", "Platform"]
    );
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
            TabScope::Project,
            TabScope::Project
        ]
    );
}

#[test]
fn test_tab_from_str_parses_persisted_values() {
    // Arrange
    let values = [
        ("Projects", Some(Tab::Projects)),
        ("Sessions", Some(Tab::Sessions)),
        ("Settings", Some(Tab::Settings)),
        ("Remote", Some(Tab::Remote)),
        ("Platform", Some(Tab::Platform)),
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
    assert_eq!(
        values,
        ["Projects", "Sessions", "Settings", "Remote", "Platform"]
    );
}

#[test]
fn test_tab_manager_offers_remote_and_platform_only_once_each_is_enabled() {
    // Arrange
    let mut remote_first = TabManager::default();
    let mut platform_only = TabManager::default();

    // Act
    let default_tabs = remote_first.available_tabs();
    remote_first.enable_remote();
    let remote_tabs = remote_first.available_tabs();
    remote_first.enable_platform();
    let both_tabs = remote_first.available_tabs();
    platform_only.enable_platform();
    let platform_tabs = platform_only.available_tabs();

    // Assert
    assert_eq!(default_tabs, [Tab::Projects, Tab::Sessions, Tab::Settings]);
    assert_eq!(
        remote_tabs,
        [Tab::Projects, Tab::Sessions, Tab::Settings, Tab::Remote]
    );
    assert_eq!(both_tabs, Tab::ALL);
    assert_eq!(
        platform_tabs,
        [Tab::Projects, Tab::Sessions, Tab::Settings, Tab::Platform]
    );
}

#[test]
fn test_tab_manager_cycles_through_remote_once_enabled() {
    // Arrange
    let mut manager = TabManager::new(Tab::Settings);
    manager.enable_remote();

    // Act
    manager.next();
    let after_settings = manager.current();
    manager.next();
    let after_remote = manager.current();
    manager.previous();
    let before_projects = manager.current();

    // Assert
    assert_eq!(after_settings, Tab::Remote);
    assert_eq!(after_remote, Tab::Projects);
    assert_eq!(before_projects, Tab::Remote);
}

#[test]
fn test_tab_manager_reads_unoffered_remote_as_sessions() {
    // Arrange
    let mut manager = TabManager::new(Tab::Remote);

    // Act
    let restored_tab = manager.current();
    manager.next();
    let next_tab = manager.current();
    manager.set(Tab::Remote);
    manager.previous();
    let previous_tab = manager.current();
    manager.set(Tab::Remote);
    manager.enable_remote();
    let enabled_tab = manager.current();

    // Assert
    assert_eq!(restored_tab, Tab::Sessions);
    assert_eq!(next_tab, Tab::Settings);
    assert_eq!(previous_tab, Tab::Projects);
    assert_eq!(enabled_tab, Tab::Remote);
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
