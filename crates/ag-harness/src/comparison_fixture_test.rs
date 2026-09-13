use std::path::Path;
use std::process::Stdio;

use tempfile::TempDir;
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use super::{ComparisonBase, ComparisonIdentity};
use crate::repository::Repository;
use crate::repository::support::test_git_executable;

pub(crate) const COMPARISON_OID: &str = "1111111111111111111111111111111111111111";

impl ComparisonBase {
    pub(crate) fn fixture(root: impl AsRef<Path>) -> Self {
        Self {
            identity: ComparisonIdentity {
                oid: COMPARISON_OID.to_string(),
                repository_root: root.as_ref().as_os_str().as_encoded_bytes().to_vec(),
            },
        }
    }
}

pub(crate) struct ComparisonRepository {
    pub(crate) base: String,
    pub(crate) blob: String,
    pub(crate) directory: TempDir,
    pub(crate) next: String,
    pub(crate) repository: Repository,
    pub(crate) tag: String,
    pub(crate) tree: String,
}

impl ComparisonRepository {
    pub(crate) async fn new() -> Self {
        let directory = tempfile::tempdir().expect("repository directory");
        let root = directory.path();
        for path in [".git/objects", ".git/refs/heads", ".git/refs/tags", "scope"] {
            tokio::fs::create_dir_all(root.join(path))
                .await
                .expect("repository layout");
        }
        tokio::fs::write(root.join(".git/HEAD"), "ref: refs/heads/release\n")
            .await
            .expect("HEAD");
        tokio::fs::write(
            root.join(".git/config"),
            "[core]\nrepositoryformatversion = 0\nbare = false\n",
        )
        .await
        .expect("config");
        let blob = Self::object(root, "blob", b"base\n").await;
        let tree = Self::tree(root, &[("100644", "name.txt", &blob)]).await;
        let base_tree = Self::tree(
            root,
            &[("100644", "outside.txt", &blob), ("40000", "scope", &tree)],
        )
        .await;
        let base = Self::commit(root, &base_tree, None).await;
        let next_blob = Self::object(root, "blob", b"next\n").await;
        let next_scope = Self::tree(root, &[("100644", "name.txt", &next_blob)]).await;
        let next_tree = Self::tree(
            root,
            &[
                ("100644", "outside.txt", &next_blob),
                ("40000", "scope", &next_scope),
            ],
        )
        .await;
        let next = Self::commit(root, &next_tree, Some(&base)).await;
        let tag = Self::object(
            root,
            "tag",
            format!(
                "object {base}\ntype commit\ntag v1\ntagger Fixture <fixture@example.test> 1 \
                 +0000\n\nrelease\n"
            )
            .as_bytes(),
        )
        .await;
        for (path, content) in [
            (".git/refs/heads/release", base.as_str()),
            (".git/refs/tags/v1", tag.as_str()),
            ("scope/name.txt", "working\n"),
            ("outside.txt", "outside work\n"),
        ] {
            tokio::fs::write(root.join(path), content)
                .await
                .expect("fixture file");
        }
        let repository =
            Repository::new(root, test_git_executable()).expect("validated repository");

        Self {
            base,
            blob,
            directory,
            next,
            repository,
            tag,
            tree,
        }
    }

    pub(crate) async fn move_branch(&self) {
        tokio::fs::write(
            self.directory.path().join(".git/refs/heads/release"),
            &self.next,
        )
        .await
        .expect("move fixture branch");
    }

    async fn commit(root: &Path, tree: &str, parent: Option<&str>) -> String {
        let parent = parent.map_or_else(String::new, |oid| format!("parent {oid}\n"));
        let content = format!(
            "tree {tree}\n{parent}author Fixture <fixture@example.test> 1 +0000\ncommitter \
             Fixture <fixture@example.test> 1 +0000\n\nfixture\n"
        );

        Self::object(root, "commit", content.as_bytes()).await
    }

    async fn tree(root: &Path, entries: &[(&str, &str, &str)]) -> String {
        let mut content = Vec::new();
        for (mode, name, oid) in entries {
            content.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            for index in (0..oid.len()).step_by(2) {
                content.push(u8::from_str_radix(&oid[index..index + 2], 16).expect("hex oid"));
            }
        }

        Self::object(root, "tree", &content).await
    }

    async fn object(root: &Path, kind: &str, content: &[u8]) -> String {
        let mut child = Command::new(test_git_executable())
            .args(["hash-object", "-t", kind, "--stdin"])
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("hash object without writing Git state");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(content)
            .await
            .expect("object input");
        let output = child.wait_with_output().await.expect("object hash");
        assert!(output.status.success());
        let oid = String::from_utf8(output.stdout)
            .expect("oid text")
            .trim()
            .to_string();
        let mut object = format!("{kind} {}\0", content.len()).into_bytes();
        object.extend_from_slice(content);
        // A stored DEFLATE block makes the fixture independent of compression
        // libraries.
        let length = u16::try_from(object.len()).expect("small fixture object");
        let mut compressed = vec![0x78, 0x01, 0x01];
        compressed.extend_from_slice(&length.to_le_bytes());
        compressed.extend_from_slice(&(!length).to_le_bytes());
        compressed.extend_from_slice(&object);
        let (mut sum, mut accumulated) = (1_u32, 0_u32);
        for byte in object {
            sum = (sum + u32::from(byte)) % 65521;
            accumulated = (accumulated + sum) % 65521;
        }
        compressed.extend_from_slice(&((accumulated << 16) | sum).to_be_bytes());
        let directory = root.join(".git/objects").join(&oid[..2]);
        tokio::fs::create_dir_all(&directory)
            .await
            .expect("object directory");
        tokio::fs::write(directory.join(&oid[2..]), compressed)
            .await
            .expect("fixture object");

        oid
    }
}
