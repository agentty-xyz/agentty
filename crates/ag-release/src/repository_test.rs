//! GitHub repository URL parsing unit tests.

use crate::repository::GitHubRepository;

#[test]
fn parse_accepts_github_urls_and_canonicalizes_them() {
    // Arrange
    let cases = [
        "https://github.com/demo-org/demo-app",
        "https://github.com/demo-org/demo-app/",
        "https://github.com/demo-org/demo-app.git",
    ];

    for url in cases {
        // Act
        let repository = GitHubRepository::parse(url);

        // Assert
        assert_eq!(
            repository,
            Ok(GitHubRepository {
                name: "demo-app".to_string(),
                owner: "demo-org".to_string(),
                url: "https://github.com/demo-org/demo-app".to_string(),
            }),
            "{url}"
        );
    }
}

#[test]
fn parse_rejects_urls_that_do_not_name_one_github_repository() {
    // Arrange
    let cases = [
        "https://gitlab.com/demo-org/demo-app",
        "http://github.com/demo-org/demo-app",
        "https://github.com/demo-org",
        "https://github.com//demo-app",
        "https://github.com/demo-org/",
        "https://github.com/demo-org/demo-app/tree/main",
        "https://github.com/demo-org/..",
        "https://github.com/./demo-app",
        "https://github.com/demo org/demo-app",
    ];

    for url in cases {
        // Act
        let repository = GitHubRepository::parse(url);

        // Assert
        assert_eq!(
            repository,
            Err(format!(
                "repository `{url}` must be a `https://github.com/<owner>/<name>` URL"
            )),
            "{url}"
        );
    }
}
