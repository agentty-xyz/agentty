//! Open tool handlers and the ordered, name-unique tool sets a turn offers.
//!
//! Every tool a turn advertises is a [`ToolHandler`]: the built-in `read`,
//! `write`, and `bash` tools are handlers too. A [`ToolSet`] lists the
//! handlers in the order their definitions are sent to the model, and a
//! turn offers exactly the tools its set holds.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::cancellation::TurnControl;
use crate::event::{RunId, SessionId};
use crate::tool::{Builtin, ToolDefinition};
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

    /// Returns what a call may change: with an approval handler, a call
    /// that is not [`ToolEffects::ReadOnly`] waits for its decision.
    fn effects(&self) -> ToolEffects;

    /// Returns what the call with the model's JSON `arguments` may change,
    /// which the approval handler sees and decides on; [`Self::effects`]
    /// unless the handler tells calls apart, as the built-in `bash` marks
    /// commands that run outside the sandbox
    /// [`ToolEffects::ExecutesUnsandboxed`].
    fn call_effects(&self, arguments: &Value) -> ToolEffects {
        let _ = arguments;

        self.effects()
    }

    /// Returns the exact argument vector, executable first, that a call
    /// [`Self::call_effects`] marks [`ToolEffects::ExecutesUnsandboxed`]
    /// runs outside the sandbox; the approval handler sees it as
    /// [`crate::approval::ApprovalRequest::command`]. `None` unless the
    /// handler runs such calls.
    fn unsandboxed_command(&self, arguments: &Value) -> Option<Vec<String>> {
        let _ = arguments;

        None
    }

    /// Runs one call with the model's JSON `arguments`, which the harness
    /// does not validate against the definition's parameters, or with the
    /// validated ones a [`crate::approval::Decision::AllowWith`] gave.
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
///
/// With a [`crate::approval::ApprovalHandler`] configured, every call to a
/// tool that is not [`Self::ReadOnly`] waits for its decision.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
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
    /// Runs a command outside the sandbox with the harness process's full
    /// access, like a `bash` call of a program
    /// `BashConfig::with_excluded_command` excludes. It runs unsandboxed
    /// only when the approval handler decides
    /// [`crate::approval::Decision::AllowUnsandboxed`]; a plain allow runs
    /// it inside the sandbox. Show the user the exact argument vector, since
    /// the program can run any code its arguments name.
    ExecutesUnsandboxed,
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
    /// Whether the approval handler explicitly allowed the call to run
    /// outside the sandbox.
    pub(crate) unsandboxed: bool,
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
            unsandboxed: false,
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
/// A turn offers exactly the tools of its set, in this order, and runs
/// every call through the handler of its name. [`Self::builtins`] holds
/// `read`, `write`, and `bash`, which need a repository; host
/// [`ToolHandler`]s do not. The harness offers no tools until the host
/// gives it a set.
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
    Builtin(Builtin),
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
    ///
    /// The set holds `bash`, so every turn offering it needs a
    /// [`crate::bash::BashConfig`], as [`Self::with_bash`] describes. For
    /// harness defaults used without one, select
    /// `ToolSet::empty().with_read().with_write()` instead.
    pub fn builtins() -> Self {
        Self {
            entries: Builtin::ALL.into_iter().map(ToolEntry::Builtin).collect(),
        }
    }

    /// Returns this set with the built-in `read`, unless the set already
    /// holds it.
    ///
    /// Built-ins added this way keep their advertised order, `read`, `write`,
    /// then `bash`, whatever order the calls come in, so equal selections
    /// record and fingerprint alike; handlers keep their positions.
    ///
    /// Unlike [`ToolSetBuilder::read`], which rejects a second `read` with
    /// [`ToolSetError::DuplicateName`], this is idempotent: it cannot fail.
    #[must_use]
    pub fn with_read(self) -> Self {
        self.with_builtin(Builtin::Read)
    }

    /// Returns this set with the built-in `write`, unless the set already
    /// holds it; ordered and idempotent like [`Self::with_read`].
    #[must_use]
    pub fn with_write(self) -> Self {
        self.with_builtin(Builtin::Write)
    }

    /// Returns this set with the built-in `bash`, unless the set already
    /// holds it; ordered and idempotent like [`Self::with_read`].
    ///
    /// A turn offering `bash` fails unless it has a
    /// [`crate::bash::BashConfig`], which the harness defaults never carry:
    /// give it with [`crate::RunRequest::bash`] or
    /// [`crate::TurnOptions::with_bash`] for each run or turn.
    #[must_use]
    pub fn with_bash(self) -> Self {
        self.with_builtin(Builtin::Bash)
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

    /// Returns this set with `builtin` added before the first built-in that
    /// follows it in the advertised order, or last, unless the set already
    /// holds it. Built-ins so keep their advertised order whatever order they
    /// are added in, and handlers keep their positions. No handler takes a
    /// built-in's name, so the name stays unique.
    fn with_builtin(mut self, builtin: Builtin) -> Self {
        if self.permissions().allows(builtin) {
            return self;
        }

        let index = self
            .entries
            .iter()
            .position(|entry| matches!(entry, ToolEntry::Builtin(held) if *held > builtin))
            .unwrap_or(self.entries.len());
        self.entries.insert(index, ToolEntry::Builtin(builtin));

        self
    }

    /// Returns whether the set holds any built-in tool, which needs a
    /// repository.
    pub(crate) fn has_builtin(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| matches!(entry, ToolEntry::Builtin(_)))
    }

    /// Returns the built-ins the set holds, as durable records and request
    /// fingerprints store them.
    pub(crate) fn permissions(&self) -> BuiltinPermissions {
        self.entries.iter().fold(
            BuiltinPermissions::default(),
            |permissions, entry| match entry {
                ToolEntry::Builtin(builtin) => permissions.allow(*builtin),
                ToolEntry::Handler { .. } => permissions,
            },
        )
    }

    /// Returns this set without the built-ins `allowed` denies.
    pub(crate) fn restricted_to(&self, allowed: BuiltinPermissions) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .filter(|entry| match entry {
                    ToolEntry::Builtin(builtin) => allowed.allows(*builtin),
                    ToolEntry::Handler { .. } => true,
                })
                .cloned()
                .collect(),
        }
    }

    /// Returns whether the set holds only built-ins, in their advertised
    /// order: the sets a built-in tool policy selected before tool sets
    /// existed, which durable records describe by their permissions alone.
    fn is_builtin_selection(&self) -> bool {
        *self == Self::builtins().restricted_to(self.permissions())
    }

    /// Returns the tool names in order, or `None` for a set of built-ins in
    /// their advertised order, which durable records describe by their
    /// permissions alone so earlier records stay comparable.
    pub(crate) fn recorded_names(&self) -> Option<Vec<String>> {
        (!self.is_builtin_selection()).then(|| self.names().map(str::to_string).collect())
    }

    /// Returns what identifies the set in a host request, or `None` for a
    /// set of built-ins in their advertised order, which the request's
    /// permissions identify: each built-in by name and each host tool by
    /// its advertised definition, in order.
    pub(crate) fn identity(&self) -> Option<Value> {
        (!self.is_builtin_selection()).then(|| {
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
    /// Adds the built-in `read`. Like every name, it may be added once:
    /// adding it again makes [`Self::build`] return
    /// [`ToolSetError::DuplicateName`], unlike the idempotent
    /// [`ToolSet::with_read`].
    pub fn read(self) -> Self {
        self.push(ToolEntry::Builtin(Builtin::Read))
    }

    /// Adds the built-in `write`, once, like [`Self::read`].
    pub fn write(self) -> Self {
        self.push(ToolEntry::Builtin(Builtin::Write))
    }

    /// Adds the built-in `bash`, once, like [`Self::read`]; a turn offering
    /// it needs a [`crate::bash::BashConfig`], as [`ToolSet::with_bash`]
    /// describes.
    pub fn bash(self) -> Self {
        self.push(ToolEntry::Builtin(Builtin::Bash))
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
        let reserved = matches!(entry, ToolEntry::Handler { .. }) && Builtin::named(name).is_some();
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

/// The tools a harness offers by default: a set, or none.
#[derive(Clone, Default)]
pub(crate) struct ToolSelection {
    pub(crate) tools: Option<ToolSet>,
}

impl ToolSelection {
    /// Returns the set a turn offers: the selected set whenever there is
    /// one, otherwise no tools.
    pub(crate) fn resolve(&self) -> ToolSet {
        self.tools.clone().unwrap_or_else(ToolSet::empty)
    }
}

/// Which built-in tools a set holds, in the shape durable turn options and
/// request fingerprints have always stored.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BuiltinPermissions {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    bash: bool,
    read: bool,
    write: bool,
}

impl BuiltinPermissions {
    /// Returns these permissions with `builtin` allowed.
    #[must_use]
    pub(crate) fn allow(mut self, builtin: Builtin) -> Self {
        *self.flag(builtin) = true;

        self
    }

    /// Returns whether `builtin` is allowed.
    pub(crate) fn allows(mut self, builtin: Builtin) -> bool {
        *self.flag(builtin)
    }

    fn flag(&mut self, builtin: Builtin) -> &mut bool {
        match builtin {
            Builtin::Bash => &mut self.bash,
            Builtin::Read => &mut self.read,
            Builtin::Write => &mut self.write,
        }
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
