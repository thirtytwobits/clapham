use std::{future::Future, pin::Pin, sync::Arc};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::{ConversionFailure, OAuthContext, Refusal};

type PolicyFuture =
    Pin<Box<dyn Future<Output = Result<Result<(), Refusal>, ConversionFailure>> + Send>>;
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
    /// Policy refusals use the operation's declared codes and categories.
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
                let input = serde_json::from_value(input).map_err(ConversionFailure::input);
                let future = input.map(|input| check(auth, input));
                Box::pin(async move {
                    Ok(future?.await.map_err(|mut refusal| {
                        refusal.evidence = crate::Evidence::NotDispatched;
                        refusal
                    }))
                })
            }),
        }
    }
    pub(crate) fn check(&self, auth: OAuthContext, input: Value) -> PolicyFuture {
        (self.check)(auth, input)
    }
}

// Serde defaults participate in compatibility because omitted input can select a resource.
pub(crate) fn compatible_input(
    policy: &Map<String, Value>,
    operation: &Map<String, Value>,
) -> bool {
    fn normalise(schema: &mut Value) {
        let Some(object) = schema.as_object_mut() else {
            return;
        };
        for keyword in [
            "title",
            "description",
            "$comment",
            "examples",
            "deprecated",
            "readOnly",
            "writeOnly",
        ] {
            object.remove(keyword);
        }
        for keyword in [
            "properties",
            "patternProperties",
            "$defs",
            "definitions",
            "dependentSchemas",
        ] {
            if let Some(children) = object.get_mut(keyword).and_then(Value::as_object_mut) {
                for child in children.values_mut() {
                    normalise(child);
                }
            }
        }
        for keyword in [
            "additionalProperties",
            "unevaluatedProperties",
            "propertyNames",
            "contains",
            "additionalItems",
            "unevaluatedItems",
            "if",
            "then",
            "else",
            "not",
            "contentSchema",
        ] {
            if let Some(child) = object.get_mut(keyword) {
                normalise(child);
            }
        }
        for keyword in ["allOf", "anyOf", "oneOf", "prefixItems"] {
            if let Some(children) = object.get_mut(keyword).and_then(Value::as_array_mut) {
                for child in children {
                    normalise(child);
                }
            }
        }
        if let Some(items) = object.get_mut("items") {
            match items {
                Value::Array(items) => {
                    for item in items {
                        normalise(item);
                    }
                }
                item => normalise(item),
            }
        }
        if let Some(dependencies) = object
            .get_mut("dependencies")
            .and_then(Value::as_object_mut)
        {
            for dependency in dependencies
                .values_mut()
                .filter(|value| value.is_object() || value.is_boolean())
            {
                normalise(dependency);
            }
        }
    }
    fn canonical(input: &Map<String, Value>) -> Value {
        let mut schema = Value::Object(input.clone());
        if schema["$schema"] == "https://json-schema.org/draft/2020-12/schema" {
            schema.as_object_mut().unwrap().remove("$schema");
        }
        normalise(&mut schema);
        schema
    }
    canonical(policy) == canonical(operation)
}
