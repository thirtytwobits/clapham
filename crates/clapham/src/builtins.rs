use std::sync::{Arc, OnceLock};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    Admission, Call, CallResult, Category, Effect, Gate, Handler, Locality, Operation, Outcome,
    Refusal, Selection,
};

/// The catalogue returned by `clapham.describe`.
///
/// Listing returns an array of descriptors. A named lookup returns one descriptor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Catalogue {
    /// Every admitted descriptor in the caller's selection, in name order.
    Operations(Vec<Operation>),
    /// The admitted descriptor requested by name.
    Operation(Box<Operation>),
}

/// The host application's identity, recorded at composition without rewriting it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationInfo {
    /// The application's name.
    pub name: String,
    /// The application's version.
    pub version: String,
}

/// The result of `clapham.version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VersionInfo {
    /// The core library's package version.
    pub clapham: String,
    /// The application's identity, where supplied at composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application: Option<ApplicationInfo>,
}

pub(crate) enum Dispatch {
    Handler(Box<dyn Handler>),
    Describe(Arc<Describe>),
    Version {
        info: VersionInfo,
        message_bytes: usize,
    },
}

impl Dispatch {
    pub(crate) fn call<'a>(
        &'a self,
        call: Call,
        selection: Option<&'a Selection>,
    ) -> crate::CheckedHandlerFuture<'a> {
        match self {
            Self::Handler(handler) => handler.call_checked(call),
            Self::Describe(describe) => Box::pin(async move { Ok(describe.call(call, selection)) }),
            Self::Version {
                info,
                message_bytes,
            } => Box::pin(async move { Ok(bounded_outcome(info, *message_bytes)) }),
        }
    }
}

pub(crate) struct Describe {
    gate: Arc<dyn Gate>,
    message_bytes: usize,
    // Only descriptors are retained, so the catalogue cannot keep handlers alive
    // through a reference cycle. Initialised before composition exposes the set.
    catalogue: OnceLock<Vec<Arc<Operation>>>,
}

impl Describe {
    pub(crate) fn new(gate: Arc<dyn Gate>, message_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            gate,
            message_bytes,
            catalogue: OnceLock::new(),
        })
    }

    pub(crate) fn initialise(&self, descriptors: Vec<Arc<Operation>>) {
        self.catalogue
            .set(descriptors)
            .expect("the composed catalogue is initialised once");
    }

    fn call(&self, call: Call, selection: Option<&Selection>) -> CallResult {
        // Low-level invoke has no selection policy, just as for other operations.
        // The public runner always supplies its authenticated selection.
        let candidates: Vec<_> = match selection {
            Some(selection) => selection.operations().collect(),
            None => self
                .catalogue
                .get()
                .expect("composition initialises the catalogue before dispatch")
                .iter()
                .map(Arc::as_ref)
                .collect(),
        };
        // Both dispatch paths validate input, rejecting null and non-string names.
        if let Some(name) = call.input.get("name").and_then(Value::as_str) {
            let operation = candidates
                .into_iter()
                .find(|operation| operation.name.as_str() == name)
                .filter(|operation| {
                    operation
                        .scopes
                        .iter()
                        .all(|scope| call.auth.has_scope(scope))
                        && self.gate.admit(&call.auth, operation) == Admission::Allow
                })
                .ok_or_else(|| {
                    Refusal::new(
                        "unknown_operation",
                        Category::NotFound,
                        "the operation is not in the admitted catalogue",
                    )
                })?;
            bounded_outcome(operation, self.message_bytes)
        } else {
            let operations: Vec<_> = candidates
                .into_iter()
                .filter(|operation| {
                    operation
                        .scopes
                        .iter()
                        .all(|scope| call.auth.has_scope(scope))
                        && self.gate.admit(&call.auth, operation) == Admission::Allow
                })
                .collect();
            bounded_outcome(&operations, self.message_bytes)
        }
    }
}

pub(crate) fn operations(
    describe: Arc<Describe>,
    application: Option<ApplicationInfo>,
) -> [(Operation, Dispatch); 2] {
    let message_bytes = describe.message_bytes;
    [
        (
            descriptor(
                "clapham.describe",
                "Describe the selected operations admitted to this caller, or one by name.",
                json!({ "type": "object", "properties": { "name": { "type": "string" } }, "additionalProperties": false }),
                crate::schema::catalogue_schema(),
                [
                    ("unknown_operation".into(), Category::NotFound),
                    ("result_too_large".into(), Category::Usage),
                ]
                .into(),
            ),
            Dispatch::Describe(describe),
        ),
        (
            descriptor(
                "clapham.version",
                "Read the library version and the application's recorded name and version.",
                json!({ "type": "object", "additionalProperties": false }),
                schemars::schema_for!(VersionInfo),
                [("result_too_large".into(), Category::Usage)].into(),
            ),
            Dispatch::Version {
                info: VersionInfo {
                    clapham: env!("CARGO_PKG_VERSION").into(),
                    application,
                },
                message_bytes,
            },
        ),
    ]
}

fn bounded_outcome(result: &impl Serialize, message_bytes: usize) -> CallResult {
    // Count through a bounded writer before allocating the structured result.
    if crate::composition::size_within(result, message_bytes).is_none() {
        return Err(Refusal::new(
            "result_too_large",
            Category::Usage,
            "the built-in result exceeds bounds.message_bytes",
        ));
    }
    Ok(Outcome::new(
        serde_json::to_value(result).expect("built-in results contain only JSON values"),
    ))
}

fn descriptor(
    name: &str,
    description: &str,
    input: Value,
    output: schemars::Schema,
    refusals: std::collections::BTreeMap<String, Category>,
) -> Operation {
    Operation {
        name: name.parse().expect("built-in operation names are valid"),
        title: None,
        description: description.into(),
        input: input.as_object().expect("input has an object root").clone(),
        output: Some(output),
        effect: Effect::Query,
        scopes: Vec::new(),
        policies: Vec::new(),
        locality: Locality::Workspace,
        refusals,
        streams: false,
        preview: false,
        positional: Vec::new(),
        confirmation: false,
        idempotent: true,
        open_world: false,
    }
}
