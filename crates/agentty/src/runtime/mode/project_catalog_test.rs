use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::handle;
use crate::presentation::app_mode::AppMode;
use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::test_support;

async fn key(app: &mut crate::app::App, code: KeyCode) {
    handle(app, KeyEvent::new(code, KeyModifiers::NONE)).await;
}

#[tokio::test]
async fn invitation_defaults_to_skip_and_navigation_does_not_scan() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::SuggestHome {
            root: PathBuf::from("/home/test"),
            selected: 1,
        },
    };
    // Act
    key(&mut app, KeyCode::Left).await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::SuggestHome { selected: 0, .. }
        }
    ));
    // Act
    key(&mut app, KeyCode::Esc).await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn path_keys_edit_submit_and_cancel() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.open_project_path_input(false);
    // Act
    key(&mut app, KeyCode::Enter).await;
    key(&mut app, KeyCode::Char('x')).await;
    key(&mut app, KeyCode::F(1)).await;
    // Assert
    assert!(
        matches!(&app.mode, AppMode::ProjectCatalog { dialog: ProjectCatalogDialog::PathInput { error: None, input, .. } } if input.text() == "x")
    );
    // Act
    key(&mut app, KeyCode::Esc).await;
    key(&mut app, KeyCode::Char('x')).await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn results_navigation_selection_save_and_cancel() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Results {
            error: None,
            paths: vec![
                (PathBuf::from("/first"), false),
                (PathBuf::from("/second"), false),
            ],
            selected: 0,
        },
    };
    // Act
    key(&mut app, KeyCode::Down).await;
    key(&mut app, KeyCode::Char(' ')).await;
    key(&mut app, KeyCode::Up).await;
    key(&mut app, KeyCode::Char('z')).await;
    // Assert
    assert!(
        matches!(&app.mode, AppMode::ProjectCatalog { dialog: ProjectCatalogDialog::Results { selected: 0, paths, .. } } if paths[1].1 && !paths[0].1)
    );
    // Act
    key(&mut app, KeyCode::Esc).await;
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Results {
            error: None,
            paths: Vec::new(),
            selected: 0,
        },
    };
    key(&mut app, KeyCode::Char(' ')).await;
    key(&mut app, KeyCode::Enter).await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn scanning_and_outcome_keys_close_only_on_explicit_cancel_or_dismiss() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Scanning {
            root: PathBuf::from("/chosen"),
        },
    };
    // Act
    key(&mut app, KeyCode::Enter).await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Scanning { .. }
        }
    ));
    // Act
    key(&mut app, KeyCode::Esc).await;
    app.project_catalog_message("No repositories found".to_string());
    key(&mut app, KeyCode::Char('x')).await;
    key(&mut app, KeyCode::Enter).await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn invitation_acceptance_without_available_home_never_scans() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::SuggestHome {
            root: PathBuf::from("/unavailable"),
            selected: 1,
        },
    };
    // Act
    key(&mut app, KeyCode::Char('y')).await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}
