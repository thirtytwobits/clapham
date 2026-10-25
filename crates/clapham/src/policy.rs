use std::{future::Future, pin::Pin, sync::Arc};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{Category, OAuthContext, Refusal};

type PolicyFuture = Pin<Box<dyn Future<Output = Result<(), Refusal>> + Send>>;
type Check = dyn Fn(OAuthContext, Value) -> PolicyFuture + Send + Sync;

/// An application resource policy bound to a typed input schema.
#[derive(Clone)]
pub struct ResourcePolicy {
    pub(crate) input: Map<String, Value>,
    check: Arc<Check>,
}

impl std::fmt::Debug for ResourcePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourcePolicy")
            .field("input", &self.input)
            .finish_non_exhaustive()
    }
}

impl ResourcePolicy {
    /// Register an asynchronous policy over verified authority and typed input.
    pub fn typed<I, F, Fut>(check: F) -> Self
    where
        I: JsonSchema + DeserializeOwned + Send + 'static,
        F: Fn(OAuthContext, I) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Refusal>> + Send + 'static,
    {
        let input = schemars::generate::SchemaSettings::draft2020_12()
            .for_deserialize()
            .into_generator()
            .into_root_schema_for::<I>()
            .as_object()
            .expect("schemars produces an object schema")
            .clone();
        Self {
            input,
            check: Arc::new(move |auth, input| {
                let input = serde_json::from_value(input).map_err(|e| {
                    Refusal::not_dispatched("invalid_input", Category::InvalidInput, e.to_string())
                });
                let future = input.map(|input| check(auth, input));
                Box::pin(async move {
                    future?.await.map_err(|mut refusal| {
                        refusal.evidence = crate::Evidence::NotDispatched;
                        refusal
                    })
                })
            }),
        }
    }
    pub(crate) fn check(&self, auth: OAuthContext, input: Value) -> PolicyFuture {
        (self.check)(auth, input)
    }
}
