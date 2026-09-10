use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::handle_project_switcher_key;
use super::support::switcher_project_item;
use crate::presentation::app_mode::AppMode;
use crate::runtime::EventResult;

#[tokio::test]
async fn test_handle_project_switcher_key_escape_returns_to_list() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 0,
    };

    // Act
    let result =
        handle_project_switcher_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_project_switcher_key_navigation_clamps_to_project_count() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    let second_project = switcher_project_item(999, "beta", base_dir.path().join("beta"), None);
    let active_project = switcher_project_item(
        app.active_project_id(),
        "alpha",
        app.projects.working_dir().to_path_buf(),
        Some(20),
    );
    app.projects
        .replace_project_items(vec![active_project, second_project]);
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 0,
    };

    // Act
    for _ in 0..3 {
        handle_project_switcher_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        )
        .await
        .expect("failed to move selection down");
    }
    let clamped_down_index = match app.mode {
        AppMode::ProjectSwitcher {
            selected_option_index,
        } => selected_option_index,
        _ => usize::MAX,
    };
    handle_project_switcher_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
    )
    .await
    .expect("failed to move selection up");

    // Assert
    assert_eq!(clamped_down_index, 1);
    assert!(matches!(
        app.mode,
        AppMode::ProjectSwitcher {
            selected_option_index: 0,
        }
    ));
}

#[tokio::test]
async fn test_handle_project_switcher_key_enter_switches_to_selected_project() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    let second_project_dir = base_dir.path().join("beta-project");
    std::fs::create_dir_all(&second_project_dir).expect("failed to create second project dir");
    let second_project_path = second_project_dir
        .canonicalize()
        .expect("failed to canonicalize second project dir");
    let second_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&second_project_path.to_string_lossy(), None)
        .await
        .expect("failed to seed second project");
    let active_project = switcher_project_item(
        app.active_project_id(),
        "alpha",
        app.projects.working_dir().to_path_buf(),
        Some(20),
    );
    let second_project =
        switcher_project_item(second_project_id, "beta-project", second_project_path, None);
    app.projects
        .replace_project_items(vec![active_project, second_project]);
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 1,
    };

    // Act
    let result =
        handle_project_switcher_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(app.active_project_id(), second_project_id);
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_handle_project_switcher_key_enter_surfaces_switch_failure() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_git_test_app().await;
    let missing_project_id = 987_654;
    let active_project = switcher_project_item(
        app.active_project_id(),
        "alpha",
        app.projects.working_dir().to_path_buf(),
        Some(20),
    );
    let missing_project = switcher_project_item(
        missing_project_id,
        "beta-project",
        base_dir.path().join("beta-project"),
        None,
    );
    app.projects
        .replace_project_items(vec![active_project, missing_project]);
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 1,
    };

    // Act
    let result =
        handle_project_switcher_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_ne!(app.active_project_id(), missing_project_id);

    let missing_project_id_text = missing_project_id.to_string();
    assert!(matches!(
        &app.mode,
        AppMode::SyncBlockedPopup {
            is_loading: false,
            message,
            project_name,
            title,
            ..
        } if title == "Project switch failed"
            && project_name.as_deref() == Some("beta-project")
            && message.contains(&missing_project_id_text)
    ));
}

#[tokio::test]
async fn test_handle_project_switcher_key_enter_on_active_project_only_closes_popup() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let active_project_id = app.active_project_id();
    app.mode = AppMode::ProjectSwitcher {
        selected_option_index: 0,
    };

    // Act
    let result =
        handle_project_switcher_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Continue)));
    assert_eq!(app.active_project_id(), active_project_id);
    assert!(matches!(app.mode, AppMode::List));
}
