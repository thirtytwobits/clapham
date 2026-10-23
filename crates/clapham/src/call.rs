use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{OperationName, Principal, Refusal};

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

/// Ordered content from an operation. References are retained without fetching them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text, unchanged.
        text: String,
    },
    /// An image reference, including a data URI for inline content.
    Image {
        /// The image's URL or data URI.
        url: String,
        /// The image's MIME type.
        mime_type: String,
    },
    /// An audio reference, including a data URI for inline content.
    Audio {
        /// The audio's URL or data URI.
        url: String,
        /// The audio's MIME type.
        mime_type: String,
    },
    /// A reference to a resource.
    ResourceLink {
        /// The resource URI.
        uri: String,
        /// The name people read.
        name: String,
        /// The MIME type, where supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    /// An embedded resource.
    Resource {
        /// The resource URI.
        uri: String,
        /// The MIME type, where supplied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// The inline resource payload.
        contents: ResourceContents,
    },
}

/// An embedded resource's text or base64-encoded bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "encoding", content = "data", rename_all = "snake_case")]
pub enum ResourceContents {
    /// Text, unchanged.
    Text(String),
    /// Base64-encoded bytes, unchanged.
    Base64(String),
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
    /// Ordered content blocks, where the operation returned content semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
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
    pub principal: Principal,
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
    pub fn new(name: OperationName, principal: Principal, input: Value) -> Self {
        Self {
            name,
            principal,
            input,
            records: RecordSink::default(),
            cancel: Cancellation::new(),
            preview: false,
        }
    }
}

/// A sendable handler future, which may borrow its handler.
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Result<Outcome, Refusal>> + Send + 'a>>;

/// An object-safe asynchronous operation handler.
pub trait Handler: Send + Sync {
    /// Run a call whose input and identity have been prepared by its dispatcher.
    fn call(&self, call: Call) -> HandlerFuture<'_>;
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
}
