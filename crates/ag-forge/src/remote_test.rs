use crate::model::{ForgeKind, ForgeRemote, ReviewRequestError};
use crate::remote::{ParsedRemote, detect_remote, display_safe_remote_url, parse_remote_url};

#[test]
fn detect_remote_returns_github_remote_for_https_origin() {
    // Arrange
    let repo_url = "https://github.com/agentty-xyz/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("github remote should be supported");

    // Assert
    assert_eq!(
        remote,
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: repo_url.to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    );
}

#[test]
fn detect_remote_ignores_https_userinfo_for_github_origin() {
    // Arrange
    let repo_url = "https://test-user:placeholder@github.com/agentty-xyz/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("github remote with https credentials should work");

    // Assert
    assert_eq!(remote.forge_kind, ForgeKind::GitHub);
    assert_eq!(remote.host, "github.com");
    assert_eq!(remote.namespace, "agentty-xyz");
    assert_eq!(remote.project, "agentty");
    assert_eq!(
        remote.repo_url,
        "https://github.com/agentty-xyz/agentty.git"
    );
    assert_eq!(remote.web_url, "https://github.com/agentty-xyz/agentty");
}

#[test]
fn detect_remote_redacts_https_userinfo_from_unsupported_remote_error() {
    // Arrange
    let repo_url = "https://test-user:placeholder@example.com/team/project.git";

    // Act
    let error = detect_remote(repo_url).expect_err("unsupported remote should fail");
    let detail = error.detail_message();

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::UnsupportedRemote {
            repo_url: "https://example.com/team/project.git".to_string(),
        }
    );
    assert!(!detail.contains("test-user"));
    assert!(!detail.contains("placeholder"));
}

#[test]
fn display_safe_remote_url_redacts_userinfo_without_a_path() {
    // Arrange
    let repo_url = "https://test-user:placeholder@example.com";

    // Act
    let sanitized = display_safe_remote_url(repo_url);

    // Assert
    assert_eq!(sanitized, "https://example.com");
}

#[test]
fn display_safe_remote_url_preserves_scp_style_remote_without_userinfo() {
    // Arrange
    let repo_url = "github.com:agentty-xyz/agentty.git";

    // Act
    let sanitized = display_safe_remote_url(repo_url);

    // Assert
    assert_eq!(sanitized, repo_url);
}

#[test]
fn detect_remote_redacts_scp_style_ssh_userinfo() {
    // Arrange
    let repo_url = "test-user@gitlab.com:agentty-xyz/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("GitLab SSH remote should be supported");

    // Assert
    assert_eq!(remote.forge_kind, ForgeKind::GitLab);
    assert_eq!(
        remote.repo_url,
        "gitlab.com:agentty-xyz/agentty.git".to_string()
    );
    assert!(!remote.repo_url.contains("test-user"));
}

#[test]
fn detect_remote_returns_github_remote_for_ssh_origin() {
    // Arrange
    let repo_url = "git@github.com:agentty-xyz/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("github ssh remote should be supported");

    // Assert
    assert_eq!(remote.forge_kind, ForgeKind::GitHub);
    assert_eq!(remote.web_url, "https://github.com/agentty-xyz/agentty");
    assert_eq!(remote.project_path(), "agentty-xyz/agentty");
}

#[test]
fn detect_remote_returns_unsupported_remote_error_for_non_forge_origin() {
    // Arrange
    let repo_url = "https://example.com/team/project.git";

    // Act
    let error = detect_remote(repo_url).expect_err("non-forge remote should be rejected");

    // Assert
    assert_eq!(
        error,
        ReviewRequestError::UnsupportedRemote {
            repo_url: repo_url.to_string(),
        }
    );
    assert!(error.detail_message().contains("GitHub and GitLab remotes"));
    assert!(error.detail_message().contains("example.com"));
}

#[test]
fn detect_remote_returns_gitlab_remote_for_https_origin() {
    // Arrange
    let repo_url = "https://gitlab.com/agentty-xyz/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("gitlab remote should be supported");

    // Assert
    assert_eq!(
        remote,
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitLab,
            host: "gitlab.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: repo_url.to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty".to_string(),
        }
    );
}

#[test]
fn detect_remote_returns_gitlab_remote_for_gitlab_subdomain_origin() {
    // Arrange
    let repo_url = "git@gitlab.company.org:team/agentty.git";

    // Act
    let remote = detect_remote(repo_url).expect("gitlab subdomain remote should be supported");

    // Assert
    assert_eq!(remote.forge_kind, ForgeKind::GitLab);
    assert_eq!(remote.host, "gitlab.company.org");
    assert_eq!(remote.project_path(), "team/agentty");
    assert_eq!(remote.web_url, "https://gitlab.company.org/team/agentty");
}

#[test]
fn parse_remote_url_rejects_missing_host_namespace_or_project() {
    // Arrange
    let invalid_remotes = [
        "",
        "https:///owner/project",
        "https://github.com/",
        "https://github.com/project",
    ];

    // Act
    let parsed = invalid_remotes.map(parse_remote_url);

    // Assert
    assert!(parsed.iter().all(Option::is_none));
}

#[test]
fn parsed_remote_rejects_empty_namespace_and_project() {
    // Arrange
    let invalid_paths = ["/project", "owner/", "owner/.git"];

    // Act
    let parsed = invalid_paths.map(|path| ParsedRemote::from_parts("", "github.com", path, false));

    // Assert
    assert!(parsed.iter().all(Option::is_none));
}
