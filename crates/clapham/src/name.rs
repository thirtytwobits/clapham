use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// An operation's name: 1 to 128 ASCII letters, digits, `_`, `-` and `.`.
///
/// A namespaced name splits at its first dot. The alphabet also admits bare names;
/// namespace ownership is checked when contributions are composed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OperationName(String);

impl OperationName {
    /// Parse a name without changing its spelling or case.
    pub fn new(name: impl Into<String>) -> Result<Self, InvalidOperationName> {
        let name = name.into();
        if !(1..=128).contains(&name.len())
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte))
        {
            return Err(InvalidOperationName);
        }
        Ok(Self(name))
    }

    /// The complete name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The namespace before the first dot, or `None` for a bare name.
    pub fn namespace(&self) -> Option<&str> {
        self.0.split_once('.').map(|(namespace, _)| namespace)
    }

    /// Everything after the first dot, or the complete bare name.
    pub fn local_name(&self) -> &str {
        self.0.split_once('.').map_or(&self.0, |(_, local)| local)
    }
}

impl FromStr for OperationName {
    type Err = InvalidOperationName;
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::new(name)
    }
}

impl TryFrom<String> for OperationName {
    type Error = InvalidOperationName;
    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}

impl From<OperationName> for String {
    fn from(name: OperationName) -> Self {
        name.0
    }
}

impl AsRef<str> for OperationName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for OperationName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl JsonSchema for OperationName {
    fn schema_name() -> Cow<'static, str> {
        "OperationName".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        // JSON Schema's `$` also matches before a final newline. The negative
        // character class closes that loophole and matches the byte parser.
        json_schema!({
            "type": "string", "minLength": 1, "maxLength": 128,
            "pattern": "^[A-Za-z0-9_.-]+$", "not": { "pattern": "[^A-Za-z0-9_.-]" }
        })
    }
}

/// A name outside the operation-name alphabet or length bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidOperationName;

impl fmt::Display for InvalidOperationName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            "an operation name must contain 1 to 128 ASCII letters, digits, '_', '-' or '.'",
        )
    }
}

impl std::error::Error for InvalidOperationName {}
