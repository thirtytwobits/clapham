use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::{Category, OperationName, PrincipalKind};

/// The MCP tool metadata key identifying an administrative effect.
pub const EFFECT_META_KEY: &str = "io.github.thirtytwobits.clapham/effect";

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

impl Effect {
    /// The kinds admitted when a descriptor omits `principals`.
    pub const fn default_principals(self) -> &'static [PrincipalKind] {
        use PrincipalKind::*;
        match self {
            Self::Query | Self::Mutation => &[Human, Agent, Service, System],
            Self::Administrative | Self::Destructive => &[Human, Service, System],
        }
    }

    /// Map the effect and the descriptor's explicit remaining hints to MCP.
    ///
    /// Queries are always idempotent. The adapter places `meta` on the MCP tool's
    /// `_meta` field, alongside its `annotations`, rather than within them.
    pub fn annotations(self, idempotent: bool, open_world: bool) -> Annotations {
        let mut meta = BTreeMap::new();
        if self == Self::Administrative {
            meta.insert(
                EFFECT_META_KEY.to_owned(),
                Value::String("administrative".into()),
            );
        }
        Annotations {
            read_only_hint: self == Self::Query,
            destructive_hint: self == Self::Destructive,
            idempotent_hint: self == Self::Query || idempotent,
            open_world_hint: open_world,
            meta,
        }
    }
}

/// MCP hints and the tool metadata needed to preserve the effect class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Annotations {
    /// The operation changes nothing.
    pub read_only_hint: bool,
    /// The operation may destroy or overwrite state.
    pub destructive_hint: bool,
    /// Repeating a call with the same arguments changes nothing more.
    pub idempotent_hint: bool,
    /// The operation reaches entities outside its own domain.
    pub open_world_hint: bool,
    /// Metadata to place on the tool itself, not inside its MCP annotations.
    #[serde(rename = "_meta", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, Value>,
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
/// Omitting `principals` admits the effect's default kinds. An explicit empty list
/// admits no kinds. The remaining MCP hints, `idempotent` and `open_world`, are
/// required rather than silently defaulted. Composition compiles the schemas and
/// checks the descriptor's cross-field semantics before calls can run.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(transform = optional_principals)]
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
    /// The kinds admitted; when omitted, defaults from `effect`.
    pub principals: Vec<PrincipalKind>,
    /// Where the operation runs.
    pub locality: Locality,
    /// Each declared refusal code and the category front ends act on.
    #[serde(default)]
    pub refusals: BTreeMap<String, Category>,
    /// Whether typed records precede the result.
    #[serde(default)]
    pub streams: bool,
    /// A query's resource URI template, where supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// Whether the operation accepts a dry run.
    #[serde(default)]
    pub preview: bool,
    /// Input properties taken as positional command-line arguments, in order.
    #[serde(default)]
    pub positional: Vec<String>,
    /// The declared MCP idempotency hint; queries always map to true.
    pub idempotent: bool,
    /// The declared MCP open-world hint.
    pub open_world: bool,
}

impl Operation {
    /// This descriptor's MCP hints and effect metadata.
    pub fn annotations(&self) -> Annotations {
        self.effect.annotations(self.idempotent, self.open_world)
    }
}

// Serde defaults cannot depend on another field. Keep the wire's optional list
// until the effect is known, then materialise it in the public descriptor.
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
    #[serde(default, deserialize_with = "deserialize_principals")]
    principals: Option<Vec<PrincipalKind>>,
    locality: Locality,
    #[serde(default)]
    refusals: BTreeMap<String, Category>,
    #[serde(default)]
    streams: bool,
    resource: Option<String>,
    #[serde(default)]
    preview: bool,
    #[serde(default)]
    positional: Vec<String>,
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
            principals: wire
                .principals
                .unwrap_or_else(|| wire.effect.default_principals().to_vec()),
            locality: wire.locality,
            refusals: wire.refusals,
            streams: wire.streams,
            resource: wire.resource,
            preview: wire.preview,
            positional: wire.positional,
            idempotent: wire.idempotent,
            open_world: wire.open_world,
        })
    }
}

fn deserialize_principals<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<PrincipalKind>>, D::Error> {
    // Missing is defaulted, but an explicit null is not a list of kinds.
    Vec::deserialize(deserializer).map(Some)
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

fn optional_principals(schema: &mut Schema) {
    if let Some(Value::Array(required)) = schema.get_mut("required") {
        required.retain(|name| name != "principals");
    }
}
