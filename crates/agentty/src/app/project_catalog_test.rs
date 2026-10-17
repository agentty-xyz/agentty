use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tempfile::tempdir;

use super::{ProjectCatalog, ProjectDiscoveryCompletion};
use crate::app::{App, AppEvent, Tab};
use crate::domain::input::InputState;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::project_discovery::{
    MockProjectDiscoveryClient, ProjectDiscoveryError, RealProjectDiscoveryClient,
};
use crate::presentation::app_mode::AppMode;
use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::test_support;

fn input_dialog(path: String, discover: bool) -> AppMode {
    AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::PathInput {
            discover,
            error: None,
            input: InputState::with_text(path),
        },
    }
}

#[tokio::test]
async fn relative_worktree_root_excludes_managed_projects_from_add_and_discovery() {
    // Arrange
    fs::create_dir_all("target").expect("local fixture parent");
    let temp = tempfile::Builder::new()
        .tempdir_in("target")
        .expect("fixture");
    let root = temp.path().canonicalize().expect("root");
    let managed = root.join("managed");
    let session = managed.join("session");
    fs::create_dir_all(session.join(".git")).expect("session");
    let current_dir = std::env::current_dir().expect("working directory");
    let relative = managed
        .strip_prefix(&current_dir)
        .expect("workspace-local fixture")
        .to_owned();
    // Act / Assert
    assert_managed_worktree_excluded(relative, &root, &session).await;
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_worktree_root_excludes_managed_projects_from_add_and_discovery() {
    // Arrange
    let temp = tempdir().expect("fixture");
    let root = temp.path().canonicalize().expect("root");
    let managed = root.join("managed");
    let session = managed.join("session");
    fs::create_dir_all(session.join(".git")).expect("session");
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&managed, &alias).expect("managed-root alias");
    // Act / Assert
    assert_managed_worktree_excluded(alias, &root, &session).await;
}

async fn assert_managed_worktree_excluded(base: PathBuf, root: &Path, session: &Path) {
    // Arrange
    let repo = root.join("ordinary-repo");
    tokio::fs::create_dir_all(repo.join(".git"))
        .await
        .expect("ordinary repository");
    let database = AppRepositories::in_memory().await.expect("db");
    let clients = test_support::test_app_clients()
        .with_project_discovery_client(Arc::new(RealProjectDiscoveryClient));
    let mut app = App::new_with_clients(base, repo.clone(), None, database, clients)
        .await
        .expect("startup");
    // Act
    app.mode = input_dialog(session.to_string_lossy().into_owned(), false);
    app.submit_project_path().await;
    // Assert
    assert!(matches!(&app.mode, AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::PathInput { error: Some(error), .. }
    } if error.contains("worktrees")));
    // Act
    app.start_project_discovery(root.to_path_buf()).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            &app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::Results { .. }
            }
        ) {
            let event = app.next_app_event().await.expect("scan event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("scan completes");
    // Assert
    assert!(matches!(&app.mode, AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Results { paths, .. }
    } if paths == &vec![(repo, false)]));
}

#[tokio::test]
async fn unresolved_worktree_root_blocks_registration_and_discovery() {
    for parent_is_file in [false, true] {
        // Arrange
        let temp = tempdir().expect("fixture");
        let parent = temp.path().join("blocked");
        let repo = temp.path().join("repo");
        fs::create_dir_all(&parent).expect("parent");
        fs::create_dir_all(repo.join(".git")).expect("repo");
        let mut app = App::new_with_clients(
            parent.join("wt"),
            repo.clone(),
            None,
            AppRepositories::in_memory().await.expect("db"),
            test_support::test_app_clients(),
        )
        .await
        .expect("startup");
        fs::remove_dir(&parent).expect("remove empty parent");
        if parent_is_file {
            fs::write(&parent, "not a directory").expect("block root");
        }
        // Act
        app.mode = input_dialog(repo.to_string_lossy().into_owned(), false);
        app.submit_project_path().await;
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::PathInput { error: Some(_), .. }
            }
        ));
        // Act
        app.start_project_discovery(repo).await;
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::Message { .. }
            }
        ));
    }
}

#[tokio::test]
async fn non_repository_launches_stay_hidden_after_add_switch_reload_and_restart() {
    // Arrange
    let temp = tempdir().expect("fixture");
    let root = temp.path().canonicalize().expect("canonical fixture");
    let launch = root.join("ordinary-launch");
    let later = root.join("later-launch");
    let repo = root.join("repo");
    fs::create_dir_all(&launch).expect("launch");
    fs::create_dir_all(&later).expect("later launch");
    fs::create_dir_all(repo.join(".git")).expect("repository");
    let database = AppRepositories::in_memory().await.expect("db");
    let mut app = App::new_with_clients(
        root.join("wt"),
        launch,
        None,
        database.clone(),
        test_support::test_app_clients(),
    )
    .await
    .expect("startup");
    assert_eq!(app.projects.render_parts().project_items, []);

    // Act
    app.mode = input_dialog(repo.to_string_lossy().into_owned(), false);
    app.submit_project_path().await;
    app.reload_projects().await;
    // Assert
    assert_eq!(app.projects.render_parts().project_items.len(), 1);
    let repo_id = app.projects.render_parts().project_items[0].project.id;
    assert_eq!(
        app.projects.render_parts().project_items[0].project.path,
        repo
    );

    // Act
    app.switch_project(repo_id).await.expect("switch");
    app.reload_projects().await;
    // Assert
    assert_eq!(app.projects.render_parts().project_items.len(), 1);
    drop(app);

    // Act
    let app = App::new_with_clients(
        root.join("wt"),
        later,
        None,
        database,
        test_support::test_app_clients(),
    )
    .await
    .expect("later startup");
    // Assert
    assert_eq!(app.projects.render_parts().project_items.len(), 1);
    assert_eq!(
        app.projects.render_parts().project_items[0].project.id,
        repo_id
    );
}

#[tokio::test]
async fn explicit_registration_can_promote_a_non_repository_launch_context() {
    // Arrange
    let temp = tempdir().expect("fixture");
    let root = temp.path().canonicalize().expect("canonical fixture");
    let launch = root.join("launch");
    fs::create_dir_all(&launch).expect("launch");
    let database = AppRepositories::in_memory().await.expect("db");
    let mut app = App::new_with_clients(
        root.join("wt"),
        launch.clone(),
        None,
        database,
        test_support::test_app_clients(),
    )
    .await
    .expect("startup");
    let project_id = app.active_project_id();
    assert_eq!(app.projects.render_parts().project_items, []);
    fs::create_dir_all(launch.join(".git")).expect("new repository");

    // Act
    app.mode = input_dialog(launch.to_string_lossy().into_owned(), false);
    app.submit_project_path().await;
    // Assert
    assert_eq!(app.projects.render_parts().project_items.len(), 1);
    assert_eq!(
        app.projects.render_parts().project_items[0].project.id,
        project_id
    );
}

#[tokio::test]
async fn suggestion_requires_consent_and_remembers_skip() {
    // Arrange
    let home = tempdir().expect("home");
    let clients =
        test_support::test_app_clients().with_project_discovery_home(Some(home.path().to_owned()));
    // Act
    let (mut app, _root) = test_support::new_test_app_with_clients(clients).await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::SuggestHome { selected: 1, .. }
        }
    ));
    // Act
    assert!(!app.has_visible_tick_driven_ui());
    let _snapshot = app.view_snapshot();
    app.dismiss_project_discovery_suggestion(false).await;
    app.suggest_project_discovery().await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
    assert_eq!(
        app.services
            .db()
            .settings()
            .get_setting(SettingName::ProjectDiscoveryPromptSeen)
            .await
            .expect("setting")
            .as_deref(),
        Some("true")
    );
}

#[tokio::test]
async fn consent_starts_one_scan_and_saving_requires_result_selection() {
    // Arrange
    let home = tempdir().expect("home");
    let expected_home = home.path().canonicalize().expect("canonical home");
    let first = expected_home.join("first");
    let second = expected_home.join("second");
    fs::create_dir_all(first.join(".git")).expect("first");
    fs::create_dir_all(second.join(".git")).expect("second");
    let found = vec![first.clone(), second.clone()];
    let mut client = MockProjectDiscoveryClient::new();
    client
        .expect_discover_project_paths()
        .once()
        .withf(move |root, _, cancelled| {
            root == &expected_home && !cancelled.load(Ordering::Relaxed)
        })
        .returning(move |_, _, _| {
            let found = found.clone();
            Box::pin(async move { Ok(found) })
        });
    let clients = test_support::test_app_clients()
        .with_project_discovery_home(Some(home.path().to_owned()))
        .with_project_discovery_client(Arc::new(client));
    let (mut app, _root) = test_support::new_test_app_with_clients(clients).await;
    // Act
    app.dismiss_project_discovery_suggestion(true).await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Scanning { .. }
        }
    ));
    // Act
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(
            app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::Results { .. }
            }
        ) {
            let event = app.next_app_event().await.expect("app event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("scan completion");
    // Assert
    assert_eq!(
        app.services
            .db()
            .projects()
            .load_projects_with_stats()
            .await
            .expect("rows")
            .len(),
        1
    );
    assert_eq!(app.tabs.current(), Tab::Projects);
    // Act
    if let AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Results { paths, .. },
    } = &mut app.mode
    {
        paths[0].1 = true;
    }
    app.save_discovered_projects().await;
    // Assert
    let rows = app
        .services
        .db()
        .projects()
        .load_projects_with_stats()
        .await
        .expect("rows");
    assert!(rows.iter().any(|row| row.path == first.to_string_lossy()));
    assert!(!rows.iter().any(|row| row.path == second.to_string_lossy()));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn cancelled_scan_cannot_replace_a_new_dialog() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    let token = Arc::new(AtomicBool::new(false));
    app.project_catalog.scan = Some((1, Arc::clone(&token)));
    // Act
    app.close_project_catalog();
    app.open_project_path_input(false);
    app.apply_app_events(AppEvent::ProjectDiscoveryCompleted {
        completion: ProjectDiscoveryCompletion {
            request_id: 1,
            result: Ok(vec![PathBuf::from("/late/repo")]),
        },
    })
    .await;
    // Assert
    assert!(token.load(Ordering::Relaxed));
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::PathInput { .. }
        }
    ));
}

#[tokio::test]
async fn dropping_catalog_stops_its_scan() {
    // Arrange
    let token = Arc::new(AtomicBool::new(false));
    let mut catalog = ProjectCatalog::new(Arc::new(RealProjectDiscoveryClient), None);
    catalog.scan = Some((1, Arc::clone(&token)));
    // Act
    drop(catalog);
    // Assert
    assert!(token.load(Ordering::Relaxed));
}

#[tokio::test]
async fn completion_reports_empty_and_failed_scans() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    for result in [Ok(Vec::new()), Err("scan failed".to_string())] {
        let token = Arc::new(AtomicBool::new(false));
        app.project_catalog.scan = Some((1, Arc::clone(&token)));
        // Act
        app.complete_project_discovery(ProjectDiscoveryCompletion {
            request_id: 1,
            result,
        });
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::Message { .. }
            }
        ));
        assert!(app.project_catalog.scan.is_none());
    }
}

#[tokio::test]
async fn background_discovery_failure_is_reported_to_the_active_dialog() {
    // Arrange
    let mut client = MockProjectDiscoveryClient::new();
    client
        .expect_discover_project_paths()
        .once()
        .returning(|_, _, _| {
            Box::pin(async {
                let task = tokio::spawn(std::future::pending::<()>());
                task.abort();

                Err(ProjectDiscoveryError::TaskJoin(
                    task.await.expect_err("cancelled task"),
                ))
            })
        });
    let clients = test_support::test_app_clients().with_project_discovery_client(Arc::new(client));
    let (mut app, root) = test_support::new_test_app_with_clients(clients).await;

    // Act
    app.start_project_discovery(root.path().to_owned()).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.project_catalog.scan.is_some() {
            let event = app.next_app_event().await.expect("completion event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("scan completes");

    // Assert
    assert!(matches!(&app.mode, AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Message { message }
    } if message.contains("Project discovery task failed") && message.contains("cancelled")));
}

#[tokio::test]
async fn catalog_error_preserves_other_dialogs_and_list_mode() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    let message = "original message".to_string();
    app.project_catalog_message(message.clone());

    // Act
    app.project_catalog_error("late path error".to_string());

    // Assert
    assert!(matches!(&app.mode, AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Message { message: actual }
    } if actual == &message));

    // Act
    app.close_project_catalog();
    app.project_catalog_error("late path error".to_string());

    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn adding_project_accepts_absolute_relative_and_home_paths() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    let home = tempdir().expect("home");
    let repo = home.path().join("repo");
    fs::create_dir_all(repo.join(".git")).expect("repo");
    let repo = repo.canonicalize().expect("canonical repo");
    app.project_catalog.home = Some(home.path().to_owned());
    app.projects.update_active_project_context(
        app.active_project_id(),
        "home".to_string(),
        None,
        None,
        home.path().to_owned(),
    );
    for text in [
        repo.to_string_lossy().into_owned(),
        "repo".to_string(),
        "~/repo".to_string(),
        "~".to_string(),
    ] {
        // Act
        app.mode = input_dialog(text.clone(), false);
        app.submit_project_path().await;
        // Assert
        if text == "~" {
            assert!(matches!(
                app.mode,
                AppMode::ProjectCatalog {
                    dialog: ProjectCatalogDialog::PathInput { error: Some(_), .. }
                }
            ));
        } else {
            assert!(matches!(app.mode, AppMode::List));
        }
    }
    let rows = app
        .services
        .db()
        .projects()
        .load_projects_with_stats()
        .await
        .expect("rows");
    assert_eq!(
        rows.iter()
            .filter(|row| row.path == repo.to_string_lossy())
            .count(),
        1
    );
}

#[tokio::test]
async fn path_validation_preserves_input_and_never_scans() {
    // Arrange
    let (mut app, root) = test_support::new_test_app().await;
    for text in [
        String::new(),
        "~/repo".to_string(),
        root.path().join("missing").to_string_lossy().into_owned(),
        root.path().to_string_lossy().into_owned(),
    ] {
        // Act
        app.mode = input_dialog(text.clone(), false);
        app.submit_project_path().await;
        // Assert
        assert!(
            matches!(&app.mode, AppMode::ProjectCatalog { dialog: ProjectCatalogDialog::PathInput { error: Some(_), input, .. } } if input.text() == text)
        );
    }
    // Act: non-dialog calls have no effect.
    app.mode = AppMode::List;
    app.submit_project_path().await;
    app.save_discovered_projects().await;
    // Assert
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn scan_submission_validates_only_selected_root() {
    // Arrange
    let (mut app, root) = test_support::new_test_app().await;
    let file = root.path().join("file");
    fs::write(&file, "file").expect("file");
    for path in [root.path().join("missing"), file] {
        // Act
        app.start_project_discovery(path).await;
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::ProjectCatalog {
                dialog: ProjectCatalogDialog::Message { .. }
            }
        ));
    }
}

#[tokio::test]
async fn path_submission_can_start_a_folder_scan() {
    // Arrange
    let home = tempdir().expect("home");
    let clients = test_support::test_app_clients()
        .with_project_discovery_client(Arc::new(RealProjectDiscoveryClient));
    let (mut app, _root) = test_support::new_test_app_with_clients(clients).await;
    // Act
    app.mode = input_dialog(home.path().to_string_lossy().into_owned(), true);
    app.submit_project_path().await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Scanning { .. }
        }
    ));
    // Act
    app.close_project_catalog();
}

#[tokio::test]
async fn managed_worktrees_cannot_be_registered_and_failed_results_remain_visible() {
    // Arrange
    let (mut app, root) = test_support::new_test_app().await;
    fs::create_dir_all(root.path().join("session/.git")).expect("marker");
    let paths = vec![
        (root.path().join("unselected"), false),
        (root.path().join("session"), true),
    ];
    app.mode = AppMode::ProjectCatalog {
        dialog: ProjectCatalogDialog::Results {
            error: None,
            paths: paths.clone(),
            selected: 1,
        },
    };
    // Act
    app.save_discovered_projects().await;
    // Assert
    assert!(matches!(&app.mode, AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Results { error: Some(error), paths: actual, selected: 1 }
        } if error.contains("worktrees") && actual == &paths));
}

#[tokio::test]
async fn database_failure_is_shown_in_suggestion_and_registration() {
    // Arrange
    let (mut app, _root, pool) = test_support::new_git_test_app_with_pool().await;
    let repo = tempdir().expect("repo");
    fs::create_dir_all(repo.path().join(".git")).expect("marker");
    pool.close().await;
    // Act
    app.dismiss_project_discovery_suggestion(false).await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::Message { .. }
        }
    ));
    // Act
    app.mode = input_dialog(repo.path().to_string_lossy().into_owned(), false);
    app.submit_project_path().await;
    // Assert
    assert!(matches!(
        app.mode,
        AppMode::ProjectCatalog {
            dialog: ProjectCatalogDialog::PathInput { error: Some(_), .. }
        }
    ));
}
