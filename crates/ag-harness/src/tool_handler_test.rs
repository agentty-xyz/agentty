//! Tool handler, context, output, error, and tool set unit tests.

use std::time::Duration;

use serde_json::json;

use super::*;

/// A host tool named `name` that echoes its arguments.
struct Echo(&'static str);

#[async_trait]
impl ToolHandler for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(self.0, "Echo the arguments.", json!({"type": "object"}))
            .expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    async fn call(&self, arguments: Value, _context: ToolContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(arguments.to_string()))
    }
}

#[tokio::test]
async fn a_host_handler_runs_outside_a_turn() {
    // Arrange
    let handler = Echo("echo");

    // Act
    let output = handler
        .call(json!({"text": "hi"}), ToolContext::new("call_1"))
        .await
        .expect("output");

    // Assert
    assert_eq!(output.content(), r#"{"text":"hi"}"#);
    assert_eq!(handler.effects(), ToolEffects::ReadOnly);
}

#[test]
fn builtins_are_ordered_read_write_bash_and_are_the_default() {
    // Arrange
    let builtins = ToolSet::builtins();

    // Act
    let names = builtins.names().collect::<Vec<_>>();

    // Assert
    assert_eq!(names, ["read", "write", "bash"]);
    assert_eq!(builtins.len(), 3);
    assert!(!builtins.is_empty());
    assert_eq!(ToolSet::default(), builtins);
    assert!(builtins.includes(Tool::Bash));
    assert!(ToolSet::empty().is_empty());
    assert!(!ToolSet::empty().includes(Tool::Read));
    assert_eq!(format!("{builtins:?}"), r#"["read", "write", "bash"]"#);
}

#[test]
fn builder_keeps_insertion_order_and_compares_handlers_by_instance() {
    // Arrange
    let echo: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));
    let other: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));

    // Act
    let set = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .builtin(Tool::Read)
        .build()
        .expect("tool set");
    let same = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .builtin(Tool::Read)
        .build()
        .expect("tool set");
    let other_instance = ToolSet::builder()
        .handler(other)
        .builtin(Tool::Read)
        .build()
        .expect("tool set");
    let other_builtin = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .builtin(Tool::Write)
        .build()
        .expect("tool set");
    let reordered = ToolSet::builder()
        .builtin(Tool::Read)
        .handler(echo)
        .build()
        .expect("tool set");

    // Assert
    assert_eq!(set.names().collect::<Vec<_>>(), ["echo", "read"]);
    assert!(!set.includes(Tool::Write));
    assert_eq!(set, same);
    assert_ne!(set, other_instance);
    assert_ne!(set, other_builtin);
    assert_ne!(set, reordered);
    assert_ne!(set, ToolSet::empty());
}

#[test]
fn builder_reports_the_first_duplicate_or_reserved_name() {
    // Arrange / Act
    let duplicate = ToolSet::builder()
        .handler(Arc::new(Echo("echo")))
        .handler(Arc::new(Echo("echo")))
        .handler(Arc::new(Echo("read")))
        .build();
    let duplicate_builtin = ToolSet::builder()
        .builtin(Tool::Bash)
        .builtin(Tool::Bash)
        .build();
    let reserved = ToolSet::builder().handler(Arc::new(Echo("write"))).build();

    // Assert
    let duplicate = duplicate.expect_err("duplicate name");
    assert_eq!(
        duplicate,
        ToolSetError::DuplicateName {
            name: "echo".to_string()
        }
    );
    assert_eq!(duplicate.to_string(), "tool `echo` was added twice");
    assert_eq!(
        duplicate_builtin.expect_err("duplicate built-in"),
        ToolSetError::DuplicateName {
            name: "bash".to_string()
        }
    );
    let reserved = reserved.expect_err("reserved name");
    assert_eq!(
        reserved,
        ToolSetError::ReservedName {
            name: "write".to_string()
        }
    );
    assert_eq!(
        reserved.to_string(),
        "tool name `write` is reserved for a built-in tool"
    );
}

#[tokio::test]
async fn a_context_outside_a_run_names_only_its_call_and_workspace() {
    // Arrange
    let context = ToolContext::new("call_1").with_workspace("/workspace");

    // Act
    let cancelled = tokio::time::timeout(Duration::ZERO, context.cancelled()).await;

    // Assert
    assert_eq!(context.call_id(), "call_1");
    assert_eq!(context.workspace(), Some(Path::new("/workspace")));
    assert_eq!(context.run_id(), None);
    assert_eq!(context.session_id(), None);
    assert!(cancelled.is_err(), "a context without a turn never cancels");
    assert_eq!(
        format!("{context:?}"),
        r#"ToolContext { call_id: "call_1", run: None, session: None, workspace: Some("/workspace"), .. }"#
    );
}

#[test]
fn output_carries_its_content_and_no_built_in_activity() {
    // Arrange / Act
    let output = ToolOutput::new("result");

    // Assert
    assert_eq!(output.content(), "result");
    assert_eq!(output.into_parts(), ("result".to_string(), None));
}

#[test]
fn errors_describe_rejections_failures_and_built_in_turn_errors() {
    // Arrange
    let rejected = ToolError::rejected("path is required");
    let failed = ToolError::failed("connection refused");
    let turn = ToolError::turn(TurnError::RepositoryRequired);

    // Act / Assert
    assert!(rejected.is_rejected());
    assert!(!failed.is_rejected());
    assert!(!turn.is_rejected());
    assert_eq!(rejected.to_string(), "tool call rejected: path is required");
    assert_eq!(failed.to_string(), "tool failed: connection refused");
    assert_eq!(turn.to_string(), TurnError::RepositoryRequired.to_string());
    assert!(rejected.source().is_none());
    assert_eq!(
        failed.source().map(ToString::to_string).as_deref(),
        Some("connection refused")
    );
    assert!(turn.source().is_some());
}
