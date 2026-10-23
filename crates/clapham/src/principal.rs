use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The kind of caller running an operation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// A person using the host.
    Human,
    /// An agent acting under the host's authority.
    Agent,
    /// A service authenticated by the host.
    Service,
    /// The host acting for itself.
    System,
}

/// An authenticated caller, serialised as `{ "id": ..., "kind": ... }`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    /// The identity assigned by the host.
    pub id: String,
    /// The caller's kind.
    pub kind: PrincipalKind,
}

impl Principal {
    /// A caller with the host's identity and kind.
    pub fn new(id: impl Into<String>, kind: PrincipalKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }
}
