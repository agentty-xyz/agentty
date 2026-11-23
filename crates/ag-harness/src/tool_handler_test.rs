//! Tool handler, context, output, error, and tool set unit tests.

use std::sync::atomic::{AtomicUsize, Ordering};
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
fn builtins_are_ordered_read_write_bash() {
    // Arrange
    let builtins = ToolSet::builtins();

    // Act
    let names = builtins.names().collect::<Vec<_>>();
    let built = ToolSet::builder()
        .read()
        .write()
        .bash()
        .build()
        .expect("tool set");

    // Assert
    assert_eq!(names, ["read", "write", "bash"]);
    assert_eq!(built, builtins);
    assert_eq!(builtins.len(), 3);
    assert!(!builtins.is_empty());
    assert!(builtins.has_builtin());
    assert!(ToolSet::empty().is_empty());
    assert!(!ToolSet::empty().has_builtin());
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
        .read()
        .build()
        .expect("tool set");
    let same = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .read()
        .build()
        .expect("tool set");
    let other_instance = ToolSet::builder()
        .handler(other)
        .read()
        .build()
        .expect("tool set");
    let other_builtin = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .write()
        .build()
        .expect("tool set");
    let reordered = ToolSet::builder()
        .read()
        .handler(echo)
        .build()
        .expect("tool set");

    // Assert
    assert_eq!(set.names().collect::<Vec<_>>(), ["echo", "read"]);
    assert!(!set.permissions().allows(Builtin::Write));
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
    let duplicate_builtin = ToolSet::builder().bash().bash().build();
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

/// A host tool whose definition names a different tool each time it is
/// read: `first`, then `read`.
struct Shifting(AtomicUsize);

#[async_trait]
impl ToolHandler for Shifting {
    fn definition(&self) -> ToolDefinition {
        let name = ["first", "read"][self.0.fetch_add(1, Ordering::SeqCst).min(1)];

        ToolDefinition::new(name, "Shift.", json!({"type": "object"})).expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    async fn call(
        &self,
        _arguments: Value,
        _context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("{}"))
    }
}

#[test]
fn a_set_keeps_the_definition_read_when_the_handler_was_added() {
    // Arrange
    let shifting = Arc::new(Shifting(AtomicUsize::new(0)));

    // Act
    let set = ToolSet::builder()
        .handler(Arc::clone(&shifting) as Arc<dyn ToolHandler>)
        .read()
        .build()
        .expect("tool set");

    // Assert
    assert_eq!(shifting.definition().name(), "read", "a later read shifts");
    assert_eq!(set.names().collect::<Vec<_>>(), ["first", "read"]);
}

#[test]
fn a_set_extends_another_through_its_builder() {
    // Arrange
    let echo: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));

    // Act
    let extended = ToolSet::builtins()
        .to_builder()
        .handler(Arc::clone(&echo))
        .build()
        .expect("tool set");
    let duplicate = extended.to_builder().write().build();

    // Assert
    assert_eq!(
        extended.names().collect::<Vec<_>>(),
        ["read", "write", "bash", "echo"]
    );
    assert_eq!(
        duplicate.expect_err("duplicate"),
        ToolSetError::DuplicateName {
            name: "write".to_string()
        }
    );
}

#[test]
fn with_builtin_methods_add_each_built_in_once_in_advertised_order() {
    // Arrange
    let echo: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));
    let hosted = ToolSet::builder()
        .handler(Arc::clone(&echo))
        .write()
        .build()
        .expect("tool set");

    // Act
    let selected = ToolSet::empty().with_read().with_write().with_bash();
    let reversed = ToolSet::empty().with_bash().with_write().with_read();
    let repeated = ToolSet::empty().with_read().with_read();
    let extended = hosted.with_bash().with_read();

    // Assert
    assert_eq!(selected, ToolSet::builtins());
    // Call order changes neither the set nor how it is recorded.
    assert_eq!(reversed, ToolSet::builtins());
    assert_eq!(reversed.recorded_names(), None);
    assert_eq!(repeated.names().collect::<Vec<_>>(), ["read"]);
    assert_eq!(
        extended.names().collect::<Vec<_>>(),
        ["echo", "read", "write", "bash"]
    );
}

#[test]
fn permissions_list_the_built_ins_a_set_holds_and_restrict_it() {
    // Arrange
    let echo: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));
    let set = ToolSet::builder()
        .bash()
        .handler(echo)
        .read()
        .build()
        .expect("tool set");
    let read_only = BuiltinPermissions::default().allow(Builtin::Read);

    // Act
    let permissions = set.permissions();
    let restricted = set.restricted_to(read_only);

    // Assert
    assert_eq!(
        permissions,
        BuiltinPermissions::default()
            .allow(Builtin::Bash)
            .allow(Builtin::Read)
    );
    assert_eq!(restricted.names().collect::<Vec<_>>(), ["echo", "read"]);
    assert_eq!(
        serde_json::to_value(permissions).expect("permissions"),
        json!({"bash": true, "read": true, "write": false})
    );
    assert_eq!(
        serde_json::to_value(read_only).expect("permissions"),
        json!({"read": true, "write": false})
    );
}

#[test]
fn a_selection_offers_its_set_or_else_no_tools() {
    // Arrange
    let echo: Arc<dyn ToolHandler> = Arc::new(Echo("echo"));
    let hosted = ToolSet::builder()
        .handler(echo)
        .write()
        .build()
        .expect("tool set");
    let selected = ToolSelection {
        tools: Some(hosted.clone()),
    };

    // Act
    let nothing = ToolSelection::default().resolve();
    let offered = selected.resolve();

    // Assert
    assert!(nothing.is_empty());
    assert_eq!(offered, hosted);
}

#[test]
fn only_a_set_other_than_built_ins_in_advertised_order_is_recorded() {
    // Arrange
    let set = ToolSet::builder()
        .handler(Arc::new(Echo("echo")))
        .bash()
        .build()
        .expect("tool set");
    let reordered = ToolSet::builder().bash().read().build().expect("tool set");

    // Act
    let names = set.recorded_names();
    let identity = set.identity();

    // Assert
    assert_eq!(ToolSet::builtins().recorded_names(), None);
    assert_eq!(ToolSet::builtins().identity(), None);
    assert_eq!(ToolSet::empty().with_write().recorded_names(), None);
    assert_eq!(ToolSet::empty().identity(), None);
    assert_eq!(
        reordered.recorded_names(),
        Some(vec!["bash".to_string(), "read".to_string()])
    );
    assert_eq!(names, Some(vec!["echo".to_string(), "bash".to_string()]));
    assert_eq!(
        identity,
        Some(json!([
            {
                "name": "echo",
                "description": "Echo the arguments.",
                "parameters": {"type": "object"},
            },
            {"builtin": "bash"},
        ]))
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
