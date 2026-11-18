//! Open tool handlers and the ordered, name-unique tool sets a turn offers.
//!
//! Every tool a turn advertises is a [`ToolHandler`]: the built-in `read`,
//! `write`, and `bash` tools are handlers too. A [`ToolSet`] lists the
//! handlers in the order their definitions are sent to the model.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use thiserror::Error;

use crate::cancellation::TurnControl;
use crate::event::{RunId, SessionId};
use crate::tool::{Tool, ToolDefinition};
use crate::turn::{ToolActivity, TurnError};

/// Whether a result fits the share of the context budget one result may
/// take, shared with the handler that produces it.
pub(crate) type SharedResultFits = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Executes one tool the model may call.
///
/// Add a handler to a [`ToolSet`] to offer its tool: the harness advertises
/// [`Self::definition`], and calls [`Self::call`] for each model call that
/// names it exactly, in the order the model sent them. A turn runs one call
/// at a time; dropping the turn, as cancellation does, drops the call's
/// future. A call that panics fails the turn as a tool failure.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Returns the definition advertised to the model. The set reads it
    /// once, when the handler is added, and advertises and dispatches by
    /// that copy.
    fn definition(&self) -> ToolDefinition;

    /// Returns what a call may change, for hosts that gate or schedule
    /// calls by their effects.
    fn effects(&self) -> ToolEffects;

    /// Runs one call with the model's JSON `arguments`, which the harness
    /// does not validate against the definition's parameters.
    ///
    /// The returned [`ToolOutput`] is sent to the model as the call's
    /// result.
    ///
    /// # Errors
    /// Returns [`ToolError::rejected`] to tell the model why the call did
    /// not run so it can correct it, or [`ToolError::failed`] to fail the
    /// turn.
    async fn call(&self, arguments: Value, context: ToolContext) -> Result<ToolOutput, ToolError>;
}

/// What one tool call may change.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ToolEffects {
    /// Reads state without changing it, like the built-in `read`.
    ReadOnly,
    /// Changes state outside the model's context, such as workspace files
    /// or records in another system, like the built-in `write`.
    Writes,
    /// Runs commands or other actions whose effects the harness cannot
    /// bound, like the built-in `bash`.
    Executes,
}

/// Facts about one tool call, given to its [`ToolHandler`].
#[derive(Clone)]
#[non_exhaustive]
pub struct ToolContext {
    pub(crate) call_id: String,
    pub(crate) control: Option<TurnControl>,
    /// Bounds a built-in `read` result to its share of the context budget.
    pub(crate) fits: Option<SharedResultFits>,
    pub(crate) run: Option<RunId>,
    pub(crate) session: Option<SessionId>,
    pub(crate) workspace: Option<PathBuf>,
}

impl ToolContext {
    /// Describes a call with identifier `call_id` outside any run, such as
    /// one a host makes to test its handler.
    pub fn new(call_id: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            control: None,
            fits: None,
            run: None,
            session: None,
            workspace: None,
        }
    }

    /// Sets the workspace root [`Self::workspace`] returns.
    #[must_use]
    pub fn with_workspace(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace = Some(root.into());

        self
    }

    /// Returns the provider-assigned identifier of the call.
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Returns the identifier of the [`crate::Run`] that made the call;
    /// `None` for a call outside a run, such as one from
    /// [`crate::Harness::turn`].
    pub fn run_id(&self) -> Option<&RunId> {
        self.run.as_ref()
    }

    /// Returns the identifier of the host-created session whose turn made
    /// the call; `None` for a stateless [`crate::Harness::run`] or
    /// [`crate::Harness::turn`].
    pub fn session_id(&self) -> Option<&SessionId> {
        self.session.as_ref()
    }

    /// Returns the root of the harness repository, when one is configured.
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Resolves once the call's turn is cancelled; never resolves for a
    /// turn that cannot be cancelled. A handler that hands work to another
    /// task can use it to stop that work.
    pub async fn cancelled(&self) {
        match &self.control {
            Some(control) => control.cancelled().await,
            None => std::future::pending().await,
        }
    }
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("call_id", &self.call_id)
            .field("run", &self.run)
            .field("session", &self.session)
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

/// The result of one tool call, sent to the model as text.
///
/// The harness bounds a host handler's result to 64 KiB: a larger one is
/// dropped, and the model is told the call ran but its output was too
/// large.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct ToolOutput {
    pub(crate) activity: Option<ToolActivity>,
    content: String,
}

impl ToolOutput {
    /// Returns an output sending `content` to the model, such as a JSON
    /// object's text.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            activity: None,
            content: content.into(),
        }
    }

    /// Returns the text sent to the model.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Returns a built-in tool's `content` with the activity its report
    /// lists.
    pub(crate) fn builtin(content: String, activity: ToolActivity) -> Self {
        Self {
            activity: Some(activity),
            content,
        }
    }

    /// Splits the output into its content and any built-in activity.
    pub(crate) fn into_parts(self) -> (String, Option<ToolActivity>) {
        (self.content, self.activity)
    }
}

/// Why a [`ToolHandler`] did not produce an output.
#[derive(Debug)]
#[non_exhaustive]
pub struct ToolError {
    pub(crate) kind: ToolErrorKind,
}

/// How the harness answers a [`ToolError`].
#[derive(Debug)]
pub(crate) enum ToolErrorKind {
    /// The model is told why, and the turn continues.
    Rejected(String),
    /// The turn fails with this source.
    Failed(Box<dyn Error + Send + Sync>),
    /// A built-in tool's turn failure, returned as is.
    Turn(Box<TurnError>),
}

impl ToolError {
    /// Rejects the call: the model receives `feedback` in an error result
    /// and may call the tool again. Use it for mistakes the model can fix,
    /// such as invalid arguments.
    pub fn rejected(feedback: impl Into<String>) -> Self {
        Self {
            kind: ToolErrorKind::Rejected(feedback.into()),
        }
    }

    /// Fails the turn with `source`, reported as a tool failure.
    pub fn failed(source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self {
            kind: ToolErrorKind::Failed(source.into()),
        }
    }

    /// Returns whether the model is told about this error and the turn
    /// continues.
    pub fn is_rejected(&self) -> bool {
        matches!(self.kind, ToolErrorKind::Rejected(_))
    }

    /// Wraps a built-in tool's turn failure.
    pub(crate) fn turn(error: impl Into<TurnError>) -> Self {
        Self {
            kind: ToolErrorKind::Turn(Box::new(error.into())),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ToolErrorKind::Rejected(feedback) => {
                write!(formatter, "tool call rejected: {feedback}")
            }
            ToolErrorKind::Failed(source) => write!(formatter, "tool failed: {source}"),
            ToolErrorKind::Turn(error) => error.fmt(formatter),
        }
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ToolErrorKind::Rejected(_) => None,
            ToolErrorKind::Failed(source) => Some(source.as_ref()),
            ToolErrorKind::Turn(error) => Some(error.as_ref()),
        }
    }
}

/// An ordered set of tools with unique names.
///
/// A turn advertises its tools in this order. The default,
/// [`Self::builtins`], holds `read`, `write`, and `bash`; a built-in in a
/// set is offered only while the turn's [`crate::ToolPolicy`] allows it.
/// Host [`ToolHandler`]s are offered whenever they are in the set, and do
/// not need a repository.
#[derive(Clone)]
#[non_exhaustive]
pub struct ToolSet {
    entries: Vec<ToolEntry>,
}

/// One tool of a [`ToolSet`].
#[derive(Clone)]
pub(crate) enum ToolEntry {
    /// A built-in tool, bound to the turn's repository and options when the
    /// turn starts.
    Builtin(Tool),
    /// A host handler with the definition read when it was added, which the
    /// turn advertises and dispatches by.
    Handler {
        definition: ToolDefinition,
        handler: Arc<dyn ToolHandler>,
    },
}

impl ToolEntry {
    fn name(&self) -> &str {
        match self {
            Self::Builtin(tool) => tool.name(),
            Self::Handler { definition, .. } => definition.name(),
        }
    }
}

impl ToolSet {
    /// Returns the built-in tools in their advertised order: `read`,
    /// `write`, then `bash`.
    pub fn builtins() -> Self {
        Self {
            entries: [Tool::Read, Tool::Write, Tool::Bash]
                .into_iter()
                .map(ToolEntry::Builtin)
                .collect(),
        }
    }

    /// Starts a builder holding this set's tools, to extend a default set,
    /// such as [`Self::builtins`], for one run.
    pub fn to_builder(&self) -> ToolSetBuilder {
        ToolSetBuilder {
            entries: self.entries.clone(),
            error: None,
        }
    }

    /// Returns a set with no tools.
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Starts an empty set to add tools to.
    pub fn builder() -> ToolSetBuilder {
        ToolSetBuilder {
            entries: Vec::new(),
            error: None,
        }
    }

    /// Returns the tool names in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(ToolEntry::name)
    }

    /// Returns the number of tools.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the set has no tools.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the tools in order.
    pub(crate) fn entries(&self) -> &[ToolEntry] {
        &self.entries
    }

    /// Returns whether the set holds the built-in `tool`.
    pub(crate) fn includes(&self, tool: Tool) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, ToolEntry::Builtin(builtin) if *builtin == tool))
    }

    /// Returns the tool names in order, or `None` for [`Self::builtins`],
    /// which durable records leave out so earlier records stay comparable.
    pub(crate) fn recorded_names(&self) -> Option<Vec<String>> {
        (*self != Self::builtins()).then(|| self.names().map(str::to_string).collect())
    }

    /// Returns what identifies the set in a host request, or `None` for
    /// [`Self::builtins`]: each built-in by name and each host tool by its
    /// advertised definition, in order.
    pub(crate) fn identity(&self) -> Option<Value> {
        (*self != Self::builtins()).then(|| {
            self.entries
                .iter()
                .map(|entry| match entry {
                    ToolEntry::Builtin(tool) => serde_json::json!({"builtin": tool.name()}),
                    ToolEntry::Handler { definition, .. } => serde_json::json!({
                        "name": definition.name(),
                        "description": definition.description(),
                        "parameters": definition.parameters(),
                    }),
                })
                .collect()
        })
    }
}

impl fmt::Debug for ToolSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.names()).finish()
    }
}

/// Sets are equal when they hold the same built-ins and the same handler
/// instances in the same order.
impl PartialEq for ToolSet {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len()
            && self
                .entries
                .iter()
                .zip(&other.entries)
                .all(|pair| match pair {
                    (ToolEntry::Builtin(left), ToolEntry::Builtin(right)) => left == right,
                    (
                        ToolEntry::Handler { handler: left, .. },
                        ToolEntry::Handler { handler: right, .. },
                    ) => Arc::ptr_eq(left, right),
                    _ => false,
                })
    }
}

impl Eq for ToolSet {}

/// Builds a [`ToolSet`] in the order tools are added.
#[must_use = "a builder does nothing until built"]
pub struct ToolSetBuilder {
    entries: Vec<ToolEntry>,
    error: Option<ToolSetError>,
}

impl ToolSetBuilder {
    /// Adds the built-in `read`; the turn's policy must still allow it.
    pub fn read(self) -> Self {
        self.push(ToolEntry::Builtin(Tool::Read))
    }

    /// Adds the built-in `write`; the turn's policy must still allow it.
    pub fn write(self) -> Self {
        self.push(ToolEntry::Builtin(Tool::Write))
    }

    /// Adds the built-in `bash`; the turn's policy must still allow it.
    pub fn bash(self) -> Self {
        self.push(ToolEntry::Builtin(Tool::Bash))
    }

    /// Adds a host `handler` under its definition's name, which must not
    /// name a built-in tool. The definition is read once, here.
    pub fn handler(self, handler: Arc<dyn ToolHandler>) -> Self {
        let definition = handler.definition();

        self.push(ToolEntry::Handler {
            definition,
            handler,
        })
    }

    /// Returns the set.
    ///
    /// # Errors
    /// Returns the first [`ToolSetError`]: a name added twice, or a handler
    /// named like a built-in tool.
    pub fn build(self) -> Result<ToolSet, ToolSetError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(ToolSet {
                entries: self.entries,
            }),
        }
    }

    fn push(mut self, entry: ToolEntry) -> Self {
        if self.error.is_some() {
            return self;
        }
        let name = entry.name();
        let reserved = matches!(entry, ToolEntry::Handler { .. })
            && [Tool::Bash, Tool::Read, Tool::Write]
                .into_iter()
                .any(|tool| tool.name() == name);
        if reserved {
            self.error = Some(ToolSetError::ReservedName {
                name: name.to_string(),
            });
        } else if self.entries.iter().any(|existing| existing.name() == name) {
            self.error = Some(ToolSetError::DuplicateName {
                name: name.to_string(),
            });
        } else {
            self.entries.push(entry);
        }

        self
    }
}

/// Why a [`ToolSetBuilder`] could not build its set.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ToolSetError {
    /// Two tools share a name.
    #[error("tool `{name}` was added twice")]
    DuplicateName {
        /// The repeated name.
        name: String,
    },
    /// A host handler uses the name of a built-in tool.
    #[error("tool name `{name}` is reserved for a built-in tool")]
    ReservedName {
        /// The reserved name.
        name: String,
    },
}

#[cfg(test)]
#[path = "tool_handler_test.rs"]
mod tests;
