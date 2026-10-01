//! Content-free execution spans. Hosts own provider and exporter configuration.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use opentelemetry::global;
pub use opentelemetry::trace::{FutureExt, TraceContextExt};
use opentelemetry::trace::{Link, SpanId, SpanKind, Status, Tracer};
pub use opentelemetry::{Context, KeyValue};

/// Records a content-free milestone in the current execution context.
pub fn milestone(name: &'static str) {
    Context::current().span().add_event(name, Vec::new());
}

/// Adds content-free ownership metadata to the current execution span.
pub fn current_attribute(key: &'static str, value: impl Into<opentelemetry::Value>) {
    Context::current()
        .span()
        .set_attribute(KeyValue::new(key, value));
}

/// Terminal outcome of an observed operation.
#[derive(Clone, Copy)]
pub enum Outcome {
    /// The operation completed successfully.
    Completed,
    /// The operation returned a failure.
    Failed,
    /// The operation was canceled or abandoned.
    Canceled,
}

/// Owns one span, ending it even when its future is dropped or unwinds.
pub struct Span {
    completion: OwnedSpanCompletion,
    context: Context,
    outcome: Outcome,
}

impl Span {
    /// Starts an independent workflow trace, linking the initiating operation
    /// when called from another trace.
    pub fn root(name: &'static str, attributes: Vec<KeyValue>) -> Self {
        let current = Context::current();
        let context = current.span().span_context().clone();
        let links = context
            .is_valid()
            .then(|| Link::new(context, Vec::new(), 0))
            .into_iter()
            .collect();

        Self::start(name, attributes, &Context::new(), links)
    }

    /// Starts an interval beneath the currently executing operation. A
    /// completed owned parent starts a new trace linked to that operation.
    pub fn child(name: &'static str) -> Self {
        let parent = Context::current();
        let span_context = parent.span().span_context().clone();
        if span_context.is_valid()
            && parent
                .get::<OwnedSpanCompletion>()
                .is_some_and(|completion| {
                    completion.span_id == span_context.span_id()
                        && completion.ended.load(Ordering::Acquire)
                })
        {
            return Self::start(
                name,
                Vec::new(),
                &Context::new(),
                vec![Link::new(span_context, Vec::new(), 0)],
            );
        }

        Self::start(name, Vec::new(), &parent, Vec::new())
    }

    /// Returns a context that can be carried across a queue or task spawn.
    pub fn context(&self) -> Context {
        self.context.clone()
    }

    /// Records an allowlisted attribute; never pass prompt or output content.
    pub fn attribute(&self, key: &'static str, value: impl Into<opentelemetry::Value>) {
        self.context.span().set_attribute(KeyValue::new(key, value));
    }

    /// Records a content-free milestone on this operation.
    pub fn event(&self, name: &'static str) {
        self.context.span().add_event(name, Vec::new());
    }

    /// Ends this interval with an explicit terminal outcome.
    pub fn finish(mut self, outcome: Outcome) {
        self.outcome = outcome;
    }

    /// Executes asynchronous work with this span's context.
    pub async fn scope<T>(mut self, work: impl Future<Output = T>) -> T {
        let result = work.with_context(self.context()).await;
        self.outcome = Outcome::Completed;

        result
    }

    /// Executes fallible work, preserving its result and classifying errors
    /// without exporting error messages.
    ///
    /// # Errors
    /// Returns the operation's original error without modification.
    pub async fn run<T, E>(
        mut self,
        work: impl Future<Output = Result<T, E>>,
        classify: impl FnOnce(&E) -> Outcome,
    ) -> Result<T, E> {
        let result = work.with_context(self.context()).await;
        self.outcome = result
            .as_ref()
            .map_or_else(classify, |_| Outcome::Completed);

        result
    }

    fn start(
        name: &'static str,
        attributes: Vec<KeyValue>,
        parent: &Context,
        links: Vec<Link>,
    ) -> Self {
        let tracer = global::tracer("ag-telemetry");
        let span = tracer
            .span_builder(name)
            .with_kind(SpanKind::Internal)
            .with_attributes(attributes)
            .with_links(links)
            .start_with_context(&tracer, parent);

        let context = parent.with_span(span);
        let completion = OwnedSpanCompletion {
            ended: Arc::new(AtomicBool::new(false)),
            span_id: context.span().span_context().span_id(),
        };

        Self {
            context: context.with_value(completion.clone()),
            completion,
            outcome: Outcome::Canceled,
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let span = self.context.span();
        let outcome = if std::thread::panicking() {
            span.set_status(Status::error(""));
            span.set_attribute(KeyValue::new("error.type", "panic"));
            "failed"
        } else {
            match self.outcome {
                Outcome::Completed => "completed",
                Outcome::Failed => {
                    span.set_status(Status::error(""));
                    span.set_attribute(KeyValue::new("error.type", "execution"));
                    "failed"
                }
                Outcome::Canceled => "canceled",
            }
        };
        span.set_attribute(KeyValue::new("agentty.outcome", outcome));
        span.end();
        self.completion.ended.store(true, Ordering::Release);
    }
}

/// Completion belongs to one owned span, even when a host replaces the
/// context's span.
#[derive(Clone)]
struct OwnedSpanCompletion {
    ended: Arc<AtomicBool>,
    span_id: SpanId,
}

/// Carries a root workflow and its waiting interval until worker selection.
pub struct QueuedTrace {
    root: Span,
    wait: Span,
}

impl QueuedTrace {
    /// Starts a workflow at acceptance, independently of the accepting task.
    pub fn new(name: &'static str, attributes: Vec<KeyValue>) -> Self {
        let root = Span::root(name, attributes);
        let wait = Span::start("queue.wait", Vec::new(), &root.context(), Vec::new());

        Self { root, wait }
    }

    /// Adds workflow ownership before execution.
    pub fn attribute(&self, key: &'static str, value: impl Into<opentelemetry::Value>) {
        self.root.attribute(key, value);
    }

    /// Ends waiting and transfers root ownership to the executing worker.
    pub fn selected(self) -> Span {
        self.wait.finish(Outcome::Completed);

        self.root
    }
}
