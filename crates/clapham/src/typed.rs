use std::{future::Future, marker::PhantomData};

use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    Call, Cancellation, Category, ComposedSet, Contribution, Effect, Evidence,
    InvalidOperationName, Locality, OAuthContext, Operation, Outcome, RecordSink, Refusal,
};

/// A transport-independent typed operation definition and invocation handle.
pub struct TypedOperation<I, O> {
    pub(crate) descriptor: Operation,
    marker: PhantomData<fn(I) -> O>,
}

impl<I, O> Clone for TypedOperation<I, O> {
    fn clone(&self) -> Self {
        Self {
            descriptor: self.descriptor.clone(),
            marker: PhantomData,
        }
    }
}

impl<I: JsonSchema, O: JsonSchema> TypedOperation<I, O> {
    /// Define a typed operation. Composition checks that its input is an object.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        effect: Effect,
    ) -> Result<Self, InvalidOperationName> {
        let input = schemars::generate::SchemaSettings::draft2020_12()
            .for_deserialize()
            .into_generator()
            .into_root_schema_for::<I>();
        let output = schemars::generate::SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<O>();
        Ok(Self {
            descriptor: Operation {
                name: name.into().parse()?,
                title: None,
                description: description.into(),
                input: input
                    .as_object()
                    .expect("schemars produces an object schema")
                    .clone(),
                output: Some(output),
                effect,
                scopes: Vec::new(),
                policies: Vec::new(),
                locality: Locality::Workspace,
                refusals: Default::default(),
                streams: false,
                preview: false,
                positional: Vec::new(),
                confirmation: false,
                idempotent: effect == Effect::Query,
                open_world: false,
            },
            marker: PhantomData,
        })
    }
}

impl<I, O> TypedOperation<I, O> {
    /// Set a display title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.descriptor.title = Some(title.into());
        self
    }
    /// Declare workspace or daemon execution requirements.
    pub fn locality(mut self, locality: Locality) -> Self {
        self.descriptor.locality = locality;
        self
    }
    /// Require CLI confirmation independently of permission grants.
    pub fn confirm(mut self) -> Self {
        self.descriptor.confirmation = true;
        self
    }
    /// Require an OAuth scope independently of the effect class.
    pub fn require_scope(mut self, scope: impl Into<String>) -> Self {
        self.descriptor.scopes.push(scope.into());
        self
    }
    /// Require an application-registered resource policy.
    pub fn policy(mut self, name: impl Into<String>) -> Self {
        self.descriptor.policies.push(name.into());
        self
    }
    /// Declare a handler refusal code and its category.
    pub fn refusal(mut self, code: impl Into<String>, category: Category) -> Self {
        self.descriptor.refusals.insert(code.into(), category);
        self
    }
    /// Declare preview support.
    pub fn preview(mut self, supported: bool) -> Self {
        self.descriptor.preview = supported;
        self
    }
    /// Declare ordered record production.
    pub fn streams(mut self, supported: bool) -> Self {
        self.descriptor.streams = supported;
        self
    }
    /// Set the positional input properties in order.
    pub fn positional(mut self, properties: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.descriptor.positional = properties.into_iter().map(Into::into).collect();
        self
    }
    /// Set the explicit idempotency hint.
    pub fn idempotent(mut self, hint: bool) -> Self {
        self.descriptor.idempotent = hint;
        self
    }
    /// Set the explicit open-world hint.
    pub fn open_world(mut self, hint: bool) -> Self {
        self.descriptor.open_world = hint;
        self
    }
    /// The generated descriptor.
    pub fn descriptor(&self) -> &Operation {
        &self.descriptor
    }
}

impl<I: Serialize, O: DeserializeOwned> TypedOperation<I, O> {
    /// Call through the shared runner and return the typed structured result.
    pub async fn call(
        &self,
        set: &ComposedSet,
        auth: OAuthContext,
        input: I,
    ) -> Result<O, Refusal> {
        Ok(self.call_outcome(set, auth, input).await?.result)
    }
    /// Call while preserving adapter content, error status and effect evidence.
    pub async fn call_outcome(
        &self,
        set: &ComposedSet,
        auth: OAuthContext,
        input: I,
    ) -> Result<TypedOutcome<O>, Refusal> {
        let input = serde_json::to_value(input).map_err(|e| {
            Refusal::not_dispatched("invalid_input", Category::InvalidInput, e.to_string())
        })?;
        let outcome = set
            .call(
                &set.all(),
                auth,
                self.descriptor.name.as_str(),
                input,
                RecordSink::default(),
                Cancellation::new(),
                false,
            )
            .await?;
        let result = serde_json::from_value(outcome.result.ok_or_else(|| {
            Refusal::new(
                "invalid_result",
                Category::Internal,
                "the operation returned no structured result",
            )
            .with_evidence(outcome.evidence)
        })?)
        .map_err(|e| {
            Refusal::new("invalid_result", Category::Internal, e.to_string())
                .with_evidence(outcome.evidence)
        })?;
        Ok(TypedOutcome {
            result,
            evidence: outcome.evidence,
            content: outcome.content,
            is_error: outcome.is_error,
        })
    }
}

/// Typed successful output with explicit dispatch evidence.
#[derive(Debug, Clone)]
pub struct TypedOutcome<O> {
    /// The successful structured result.
    pub result: O,
    /// Knowledge of the call's effects.
    pub evidence: Evidence,
    /// Ordered opaque adapter content.
    pub content: Option<Vec<serde_json::Value>>,
    /// Whether the adapter reports a tool-level error result.
    pub is_error: bool,
}

impl<O> TypedOutcome<O> {
    /// An ordinary completed result.
    pub fn new(result: O) -> Self {
        Self {
            result,
            evidence: Evidence::Completed,
            content: None,
            is_error: false,
        }
    }
    /// Serialise native adapter content without introducing protocol types in the core.
    pub fn with_content<C: Serialize>(
        mut self,
        content: impl IntoIterator<Item = C>,
    ) -> Result<Self, serde_json::Error> {
        self.content = Some(
            content
                .into_iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()?,
        );
        Ok(self)
    }
}

impl Contribution {
    /// Register a typed handler, deriving its wire conversion internally.
    pub fn typed_operation<I, O, F, Fut>(self, operation: TypedOperation<I, O>, handler: F) -> Self
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
        F: Fn(Call, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O, Refusal>> + Send + 'static,
    {
        self.typed_operation_with_outcome(operation, move |call, input| {
            let future = handler(call, input);
            async move { future.await.map(TypedOutcome::new) }
        })
    }

    /// Register a typed handler retaining explicit effect evidence.
    pub fn typed_operation_with_outcome<I, O, F, Fut>(
        self,
        operation: TypedOperation<I, O>,
        handler: F,
    ) -> Self
    where
        I: DeserializeOwned + Send + 'static,
        O: Serialize + Send + 'static,
        F: Fn(Call, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TypedOutcome<O>, Refusal>> + Send + 'static,
    {
        self.operation(
            operation.descriptor,
            TypedHandler::<I, O, F> {
                handler,
                marker: PhantomData,
            },
        )
    }
}

struct TypedHandler<I, O, F> {
    handler: F,
    marker: PhantomData<fn(I) -> O>,
}
impl<I, O, F, Fut> crate::Handler for TypedHandler<I, O, F>
where
    I: DeserializeOwned + Send + 'static,
    O: Serialize + Send + 'static,
    F: Fn(Call, I) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<TypedOutcome<O>, Refusal>> + Send + 'static,
{
    fn validate_conversion(
        &self,
        input: &serde_json::Value,
    ) -> Result<(), crate::ConversionFailure> {
        serde_json::from_value::<I>(input.clone())
            .map(|_| ())
            .map_err(crate::ConversionFailure::input)
    }
    fn call(&self, call: Call) -> crate::HandlerFuture<'_> {
        Box::pin(async move { self.call_checked(call).await? })
    }
    fn call_checked(&self, call: Call) -> crate::CheckedHandlerFuture<'_> {
        Box::pin(async move {
            let input = serde_json::from_value(call.input.clone())
                .map_err(crate::ConversionFailure::input)?;
            let typed = match (self.handler)(call, input).await {
                Ok(typed) => typed,
                Err(refusal) => return Ok(Err(refusal)),
            };
            let result = serde_json::to_value(typed.result)
                .map_err(|e| crate::ConversionFailure::output(e, typed.evidence))?;
            let mut outcome = Outcome::new(result);
            outcome.evidence = typed.evidence;
            outcome.content = typed.content;
            outcome.is_error = typed.is_error;
            Ok(Ok(outcome))
        })
    }
}
