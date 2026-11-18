//! Artifact naming unit tests.

use crate::artifact;
use crate::target::Target;

#[test]
fn artifact_names_follow_the_published_contract() {
    // Arrange
    let app = "demo-app";
    let target = Target::X8664UnknownLinuxGnu;

    // Act
    let names = [
        artifact::archive_directory(app, target),
        artifact::archive(app, target),
        artifact::checksum(&artifact::archive(app, target)),
        artifact::checksum(artifact::SOURCE_ARCHIVE),
        artifact::source_directory("demo-repo", "1.2.3"),
        artifact::shell_installer(app),
        artifact::npm_tarball(app),
        artifact::npm_package(app, Some("@demo-org")),
        artifact::npm_package(app, None),
        artifact::CHECKSUM_LIST.to_string(),
        artifact::MANIFEST.to_string(),
    ];

    // Assert
    assert_eq!(
        names,
        [
            "demo-app-x86_64-unknown-linux-gnu",
            "demo-app-x86_64-unknown-linux-gnu.tar.gz",
            "demo-app-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "source.tar.gz.sha256",
            "demo-repo-1.2.3",
            "demo-app-installer.sh",
            "demo-app-npm-package.tar.gz",
            "@demo-org/demo-app",
            "demo-app",
            "sha256.sum",
            "release-manifest.json",
        ]
    );
}
