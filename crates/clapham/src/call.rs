use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{OAuthContext, OperationName, Refusal};

/// What is known about an invocation's effects, independently of result delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    /// The handler or transport confirms the operation was not dispatched.
    NotDispatched,
    /// A complete result or refusal returned; this does not establish durable publication.
    Completed,
    /// The operation may have run; cancellation or a lost reply leaves its effects unresolved.
    Uncertain,
}

/// A handler's result, optional ordered content and dispatch evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    /// Structured output; an absent remote result differs from a JSON null.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_result"
    )]
    pub result: Option<Value>,
    /// Opaque ordered content supplied and interpreted by an adapter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
    /// A tool-reported execution error, distinct from a dispatch refusal.
    #[serde(default)]
    pub is_error: bool,
    /// What is known about the operation's effects.
    pub evidence: Evidence,
}

impl Outcome {
    /// A completed ordinary JSON result, including an explicit JSON null.
    pub fn new(result: Value) -> Self {
        Self {
            result: Some(result),
            content: None,
            is_error: false,
            evidence: Evidence::Completed,
        }
    }
}

fn present_result<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// A clonable, runtime-independent cancellation signal.
#[derive(Debug, Clone, Default)]
pub struct Cancellation(tokio_util::sync::CancellationToken);

impl Cancellation {
    /// A signal that has not fired.
    pub fn new() -> Self {
        Self::default()
    }
    /// Fire the signal for every clone.
    pub fn cancel(&self) {
        self.0.cancel();
    }
    /// Whether the signal has fired.
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
    /// Wait for cancellation without requiring a timer or a runtime.
    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

type RecordFuture = Pin<Box<dyn Future<Output = Result<(), Refusal>> + Send>>;
type RecordCallback = dyn Fn(Value) -> RecordFuture + Send + Sync;

/// An asynchronous record sink with backpressure supplied by the host.
#[derive(Clone)]
pub struct RecordSink(Arc<RecordCallback>);

impl RecordSink {
    /// A sink that awaits each callback before the producer can continue.
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Refusal>> + Send + 'static,
    {
        Self(Arc::new(move |record| Box::pin(callback(record))))
    }
    /// Deliver one record and await the host's acknowledgement.
    /// A sink refusal's evidence is propagated unchanged. If delivery failure leaves
    /// the operation's effects unresolved, the sink must report [`Evidence::Uncertain`].
    /// Completed evidence means a complete reply, not atomic or durable effects.
    pub async fn send(&self, record: Value) -> Result<(), Refusal> {
        (self.0)(record).await
    }
}

impl Default for RecordSink {
    /// A sink that discards records.
    fn default() -> Self {
        Self::new(|_| async { Ok(()) })
    }
}

impl fmt::Debug for RecordSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecordSink { .. }")
    }
}

/// A handler's call context. Dispatch sets `name` to the operation's own name.
#[derive(Debug, Clone)]
pub struct Call {
    /// The operation's own name, without a front end's alias.
    pub name: OperationName,
    /// The authenticated caller.
    pub auth: OAuthContext,
    /// Input validated against the operation's compiled schema before dispatch.
    pub input: Value,
    /// The host's sink for typed records.
    pub records: RecordSink,
    /// The signal that stops this call.
    pub cancel: Cancellation,
    /// Whether the call is a dry run.
    pub preview: bool,
}

impl Call {
    /// A call with a discard sink, a fresh cancellation signal and no dry run.
    pub fn new(name: OperationName, auth: OAuthContext, input: Value) -> Self {
        Self {
            name,
            auth,
            input,
            records: RecordSink::default(),
            cancel: Cancellation::new(),
            preview: false,
        }
    }
}

/// A sendable handler future, which may borrow its handler.
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<Outcome, Refusal>> + Send + 'a>>;

/// A conversion error produced by the typed adapter.
/// Application failures remain ordinary handler refusals.
#[derive(Debug)]
pub struct ConversionFailure(Refusal);
impl ConversionFailure {
    pub(crate) fn input(error: impl fmt::Display) -> Self {
        Self(Refusal::not_dispatched(
            "invalid_input",
            crate::Category::InvalidInput,
            error.to_string(),
        ))
    }
    pub(crate) fn output(error: impl fmt::Display, evidence: Evidence) -> Self {
        Self(
            Refusal::new(
                "invalid_result",
                crate::Category::Internal,
                error.to_string(),
            )
            .with_evidence(evidence),
        )
    }
}
impl From<ConversionFailure> for Refusal {
    fn from(error: ConversionFailure) -> Self {
        error.0
    }
}
impl fmt::Display for ConversionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ConversionFailure {}

/// A typed conversion failure or an ordinary handler result/refusal.
pub type CheckedHandlerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Result<Outcome, Refusal>, ConversionFailure>> + Send + 'a>>;

/// A transport-independent asynchronous operation handler.
pub trait Handler: Send + Sync {
    /// Run a call whose input and identity have been prepared by its dispatcher.
    fn call(&self, call: Call) -> HandlerFuture<'_>;
    /// Separate dispatcher conversion failures from the handler's declared refusals.
    fn call_checked(&self, call: Call) -> CheckedHandlerFuture<'_> {
        Box::pin(async move { Ok(self.call(call).await) })
    }
    /// Validate conversion into the handler input before admission or dispatch.
    fn validate_conversion(&self, _: &Value) -> Result<(), ConversionFailure> {
        Ok(())
    }
}

impl<F, Fut> Handler for F
where
    F: Fn(Call) -> Fut + Send + Sync,
    Fut: Future<Output = Result<Outcome, Refusal>> + Send + 'static,
{
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        Box::pin(self(call))
    }
}

impl Handler for Box<dyn Handler> {
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        self.as_ref().call(call)
    }
    fn call_checked(&self, call: Call) -> CheckedHandlerFuture<'_> {
        self.as_ref().call_checked(call)
    }
    fn validate_conversion(&self, input: &Value) -> Result<(), ConversionFailure> {
        self.as_ref().validate_conversion(input)
    }
}
