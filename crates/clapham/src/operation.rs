use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::{Category, OperationName};

/// What an operation does to its environment, declared independently of transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Reads without changing state.
    Query,
    /// Changes state within the operation's domain.
    Mutation,
    /// Changes the host's own administrative state.
    Administrative,
    /// May destroy or overwrite state.
    Destructive,
}

/// Where an operation runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    /// Runs against the workspace under its lock.
    Workspace,
    /// Needs the running daemon.
    Daemon,
}

/// An operation's language-neutral descriptor.
///
/// OAuth scopes and resource policies constrain admission independently of effects.
/// The behaviour declarations, `idempotent` and `open_world`, are
/// required rather than silently defaulted. Composition compiles the schemas and
/// checks the descriptor's cross-field semantics before calls can run.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// The operation's identity, split into namespace and local name at the first dot.
    pub name: OperationName,
    /// The name people read, where supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What the operation does; this field is required.
    pub description: String,
    /// The input JSON Schema, explicitly declaring `type: object` at its root.
    #[schemars(schema_with = "input_schema")]
    pub input: Map<String, Value>,
    /// The result JSON Schema, where supplied; object and boolean schemas are admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Schema>,
    /// What the operation does to its environment.
    pub effect: Effect,
    /// Every OAuth scope required to invoke or discover this operation.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Named resource policies, resolved at composition.
    #[serde(default)]
    pub policies: Vec<String>,
    /// Where the operation runs.
    pub locality: Locality,
    /// Each declared refusal code and the category front ends act on.
    #[serde(default)]
    pub refusals: BTreeMap<String, Category>,
    /// Whether typed records precede the result.
    #[serde(default)]
    pub streams: bool,
    /// Whether the operation accepts a dry run.
    #[serde(default)]
    pub preview: bool,
    /// Input properties taken as positional command-line arguments, in order.
    #[serde(default)]
    pub positional: Vec<String>,
    /// Whether the CLI requires explicit confirmation.
    #[serde(default)]
    pub confirmation: bool,
    /// Whether repeating the operation has the same effect as one call.
    pub idempotent: bool,
    /// Whether the operation reaches entities outside its own domain.
    pub open_world: bool,
}

// The wire parser enforces object-root input schemas before composition.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationWire {
    name: OperationName,
    title: Option<String>,
    description: String,
    #[serde(deserialize_with = "deserialize_input")]
    input: Map<String, Value>,
    output: Option<Schema>,
    effect: Effect,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    policies: Vec<String>,
    locality: Locality,
    #[serde(default)]
    refusals: BTreeMap<String, Category>,
    #[serde(default)]
    streams: bool,
    #[serde(default)]
    preview: bool,
    #[serde(default)]
    positional: Vec<String>,
    #[serde(default)]
    confirmation: bool,
    idempotent: bool,
    open_world: bool,
}

impl<'de> Deserialize<'de> for Operation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = OperationWire::deserialize(deserializer)?;
        Ok(Self {
            name: wire.name,
            title: wire.title,
            description: wire.description,
            input: wire.input,
            output: wire.output,
            effect: wire.effect,
            scopes: wire.scopes,
            policies: wire.policies,
            locality: wire.locality,
            refusals: wire.refusals,
            streams: wire.streams,
            preview: wire.preview,
            positional: wire.positional,
            confirmation: wire.confirmation,
            idempotent: wire.idempotent,
            open_world: wire.open_world,
        })
    }
}

fn deserialize_input<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Map<String, Value>, D::Error> {
    let input = Map::deserialize(deserializer)?;
    if input.get("type").and_then(Value::as_str) != Some("object") {
        return Err(serde::de::Error::custom(
            "input schema must declare type: object at its root",
        ));
    }
    Ok(input)
}

fn input_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "object",
        "properties": { "type": { "const": "object" } },
        "required": ["type"], "additionalProperties": true
    })
}
