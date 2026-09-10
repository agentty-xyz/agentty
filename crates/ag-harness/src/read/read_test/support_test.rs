use std::io;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use mockall::Sequence;
use tokio::io::{AsyncRead, ReadBuf};

use crate::file_system::{FileSystem, MockFileSystem};
use crate::read::command::{RepositoryCommandOutput, RepositoryCommandRunner};
use crate::read::runtime::ReadTool;
use crate::repository::support::test_git_executable;
use crate::tool::ReadArguments;

pub(super) struct FailingReader;

impl AsyncRead for FailingReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::other("broken stream")))
    }
}

pub(super) struct ContentThenFailReader {
    pub(super) content: Option<Vec<u8>>,
}

impl AsyncRead for ContentThenFailReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let Some(content) = self.content.take() else {
            return Poll::Ready(Err(io::Error::other("broken continuation probe")));
        };
        buffer.put_slice(&content);

        Poll::Ready(Ok(()))
    }
}

pub(super) fn arguments(mut value: serde_json::Value) -> ReadArguments {
    value
        .as_object_mut()
        .expect("read argument fixture should be an object")
        .insert("action".to_string(), serde_json::json!("file"));

    serde_json::from_value(value).expect("read arguments should be valid")
}

pub(super) fn file_system(content: impl Into<Vec<u8>>) -> Arc<MockFileSystem> {
    file_system_reader(Box::new(Cursor::new(content.into())))
}

pub(super) fn file_system_reader(reader: Box<dyn AsyncRead + Send + Unpin>) -> Arc<MockFileSystem> {
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .withf(|path| path == Path::new("repo"))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .withf(|path| path == Path::new("/repo/input.txt"))
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo/input.txt")));
    file_system
        .expect_open_beneath()
        .withf(|root, path| root == Path::new("/repo") && path == Path::new("input.txt"))
        .times(1)
        .return_once(move |_, _| Ok(reader));

    Arc::new(file_system)
}

pub(super) fn inspection_file_system() -> Arc<MockFileSystem> {
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .withf(|path| path == Path::new("repo"))
        .times(1)
        .returning(|_| Ok(PathBuf::from("/repo")));

    Arc::new(file_system)
}

pub(super) fn command_output(code: i32, stdout: impl Into<Vec<u8>>) -> RepositoryCommandOutput {
    RepositoryCommandOutput {
        code: Some(code),
        stderr: Vec::new(),
        stdout: stdout.into(),
        truncated: false,
    }
}

pub(super) fn truncated_command_output(
    code: i32,
    stdout: impl Into<Vec<u8>>,
) -> RepositoryCommandOutput {
    RepositoryCommandOutput {
        code: Some(code),
        stderr: Vec::new(),
        stdout: stdout.into(),
        truncated: true,
    }
}

impl ReadTool {
    pub(super) fn new(file_system: Arc<dyn FileSystem>, repository_root: PathBuf) -> Self {
        Self::with_git(file_system, repository_root, test_git_executable())
    }

    pub(super) fn with_command_runner(
        mut self,
        command_runner: Arc<dyn RepositoryCommandRunner>,
    ) -> Self {
        self.command_runner = command_runner;

        self
    }
}
