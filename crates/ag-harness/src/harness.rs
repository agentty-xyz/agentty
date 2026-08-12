use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;

use crate::file_system::{FileSystem, LocalFileSystem};
use crate::model::{Model, ModelError, ModelRequest, ModelResponse};
use crate::policy::Policy;
use crate::read::{ReadError, ReadTool};
use crate::schema_contract::OutputSchema;
use crate::tool::{Tool, ToolDefinition};

const DEFAULT_MAX_TOOL_CALLS: usize = 8;

/// Application-facing harness for one complete model turn.
///
/// A turn advertises policy-approved tools, executes validated native calls,
/// returns tool results to the model, and finishes with locally validated
/// structured output.
pub struct Harness {
    file_system: Arc<dyn FileSystem>,
    max_tool_calls: usize,
    model: Arc<dyn Model>,
    policy: Policy,
    repository_root: Option<PathBuf>,
}

impl Harness {
    /// Creates a deny-by-default harness backed by the local filesystem.
    pub fn new(model: impl Model + 'static) -> Self {
        Self {
            file_system: Arc::new(LocalFileSystem),
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            model: Arc::new(model),
            policy: Policy::default(),
            repository_root: None,
        }
    }

    /// Roots repository-scoped tools at `repository_root`.
    #[must_use]
    pub fn repository(mut self, repository_root: impl Into<PathBuf>) -> Self {
        self.repository_root = Some(repository_root.into());

        self
    }

    /// Enables one built-in tool for model requests.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        self.policy.allow(tool);

        self
    }

    /// Replaces the local filesystem implementation.
    #[must_use]
    pub fn file_system(mut self, file_system: impl FileSystem + 'static) -> Self {
        self.file_system = Arc::new(file_system);

        self
    }

    /// Overrides the maximum number of native calls allowed in one turn.
    #[must_use]
    pub fn max_tool_calls(mut self, max_tool_calls: NonZeroUsize) -> Self {
        self.max_tool_calls = max_tool_calls.get();

        self
    }

    /// Runs one prompt through tool execution to terminal structured output.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the call limit, or the requested repository read fails.
    pub async fn run(
        &self,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Result<Value, TurnError> {
        let mut request = ModelRequest::new(prompt, schema);
        let read_tool = if self.policy.allows(Tool::Read) {
            let repository_root = self
                .repository_root
                .as_ref()
                .ok_or(TurnError::RepositoryRequired)?;
            request = request.with_tool(ToolDefinition::read());

            Some(ReadTool::new(
                self.file_system.clone(),
                repository_root.clone(),
            ))
        } else {
            None
        };
        let mut completed_tool_calls = 0_usize;

        loop {
            match self.model.complete(request.clone()).await? {
                ModelResponse::Output(output) => return Ok(output),
                ModelResponse::ToolCall(call) => {
                    let Some(read_tool) = read_tool.as_ref() else {
                        return Err(TurnError::ToolDenied {
                            name: call.name().to_string(),
                        });
                    };
                    if completed_tool_calls >= self.max_tool_calls {
                        return Err(TurnError::ToolCallLimit {
                            limit: self.max_tool_calls,
                        });
                    }
                    let output = read_tool.execute(call.arguments()).await?;
                    let result = output.to_tool_result().map_err(ReadError::from)?;
                    request.record_tool_result(call, result);
                    completed_tool_calls += 1;
                }
            }
        }
    }
}

/// Failure returned by a complete harness turn.
#[derive(Debug, Error)]
pub enum TurnError {
    /// Provider request, response decoding, or terminal validation failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The model requested a tool unavailable under the configured policy.
    #[error("tool `{name}` is denied by policy")]
    ToolDenied {
        /// Denied native function name.
        name: String,
    },
    /// A repository read failed.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// Repository-scoped tools were enabled without a repository root.
    #[error("repository root is required when the read tool is allowed")]
    RepositoryRequired,
    /// The model exceeded the bounded number of calls in one turn.
    #[error("model exceeded the per-turn tool call limit of {limit}")]
    ToolCallLimit {
        /// Configured maximum calls.
        limit: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mockall::Sequence;
    use serde_json::json;

    use super::*;
    use crate::file_system::MockFileSystem;
    use crate::model::{MockModel, ModelMessage};
    use crate::tool::{ReadArguments, ToolCall};

    fn object_schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn read_call(id: &str) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "path": "Cargo.toml",
            "limit": 1
        }))
        .expect("read arguments should be valid");

        ToolCall::read(id.to_string(), arguments, None)
    }

    fn readable_file_system() -> MockFileSystem {
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/Cargo.toml")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| {
                Ok(Box::new(Cursor::new(
                    b"[workspace]\nmember = true\n".to_vec(),
                )))
            });

        file_system
    }

    #[tokio::test]
    async fn completes_read_tool_round_trip() {
        // Arrange
        let mut model = MockModel::new();
        let call_count = Arc::new(AtomicUsize::new(0));
        model.expect_complete().times(2).returning(move |request| {
            let call_index = call_count.fetch_add(1, Ordering::SeqCst);
            if call_index == 0 {
                assert_eq!(request.tools(), &[ToolDefinition::read()]);

                return Ok(ModelResponse::ToolCall(read_call("call_read")));
            }
            assert_eq!(request.messages().len(), 3);
            assert!(matches!(
                &request.messages()[0],
                ModelMessage::User(prompt) if prompt == "inspect the manifest"
            ));
            assert!(matches!(
                &request.messages()[1],
                ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
            ));
            assert!(matches!(
                &request.messages()[2],
                ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                }
                    if call_id == "call_read"
                        && name == "read"
                        && serde_json::from_str::<Value>(content)
                            .is_ok_and(|value| value["content"] == "[workspace]")
            ));

            Ok(ModelResponse::Output(json!({ "summary": "workspace" })))
        });
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read);

        // Act
        let output = harness
            .run("inspect the manifest", object_schema())
            .await
            .expect("tool round trip should succeed");

        // Assert
        assert_eq!(output, json!({ "summary": "workspace" }));
    }

    #[tokio::test]
    async fn requires_repository_when_read_is_allowed() {
        // Arrange
        let mut model = MockModel::new();
        model.expect_complete().times(0);
        let harness = Harness::new(model).allow(Tool::Read);

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("read should require a repository root");

        // Assert
        assert!(matches!(error, TurnError::RepositoryRequired));
    }

    #[tokio::test]
    async fn rejects_tool_call_when_policy_denies_read() {
        // Arrange
        let mut model = MockModel::new();
        model.expect_complete().times(1).returning(|request| {
            assert!(request.tools().is_empty());

            Ok(ModelResponse::ToolCall(read_call("call_denied")))
        });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo");

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("denied tool should fail");

        // Assert
        assert!(matches!(
            error,
            TurnError::ToolDenied { name } if name == "read"
        ));
    }

    #[tokio::test]
    async fn enforces_tool_call_limit() {
        // Arrange
        let mut model = MockModel::new();
        model
            .expect_complete()
            .times(2)
            .returning(|_| Ok(ModelResponse::ToolCall(read_call("call_read"))));
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("second tool call should exceed the limit");

        // Assert
        assert!(matches!(error, TurnError::ToolCallLimit { limit: 1 }));
    }

    #[tokio::test]
    async fn returns_typed_read_failure() {
        // Arrange
        let mut model = MockModel::new();
        model
            .expect_complete()
            .times(1)
            .returning(|_| Ok(ModelResponse::ToolCall(read_call("call_read"))));
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Read);

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("filesystem failure should end the turn");

        // Assert
        assert!(matches!(
            error,
            TurnError::Read(ReadError::RepositoryRoot { .. })
        ));
    }

    #[tokio::test]
    async fn returns_typed_model_failure() {
        // Arrange
        let mut model = MockModel::new();
        model
            .expect_complete()
            .times(1)
            .returning(|_| Err(ModelError::request(io::Error::other("offline"))));
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo");

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("model failure should end the turn");

        // Assert
        assert!(matches!(error, TurnError::Model(ModelError::Request(_))));
    }
}
