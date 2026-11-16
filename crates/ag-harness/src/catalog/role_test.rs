//! Role and selector unit tests for built-in and host-defined roles.

use crate::catalog::{ModelId, ModelSelector, Role};

#[test]
fn built_in_roles_name_themselves_and_hosts_add_roles() {
    // Arrange
    let built_in = [Role::SMART, Role::CHEAP, Role::FAST, Role::VISION];

    // Act
    let names = built_in.map(|role| role.to_string());
    let owned = Role::new("review".to_string());

    // Assert
    assert_eq!(names, ["smart", "cheap", "fast", "vision"]);
    assert_eq!(owned, Role::new("review"));
    assert_eq!(owned.as_str(), "review");
    assert_eq!(Role::new("cheap"), Role::CHEAP);
}

#[test]
fn selectors_convert_from_identifiers_and_roles() {
    // Arrange
    let id = ModelId::new("openai/gpt-6-luna");

    // Act
    let by_id = ModelSelector::from(id.clone());
    let by_role = ModelSelector::from(Role::FAST);

    // Assert
    assert_eq!(by_id, ModelSelector::Id(id));
    assert_eq!(by_role, ModelSelector::Role(Role::FAST));
}
