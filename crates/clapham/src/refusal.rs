use std::fmt;

use crate::Evidence;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The fixed categories front ends act on when an operation refuses a call.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// The invocation was wrong.
    Usage,
    /// The input failed its schema or the operation's checks.
    InvalidInput,
    /// What the input names is absent.
    NotFound,
    /// A fence or state conflict prevents the change.
    Conflict,
    /// A gate, policy or approval said no.
    Refused,
    /// A required workspace, daemon or remote is unavailable.
    Unavailable,
    /// The call was cancelled, timed out or stopped by shutdown.
    Interrupted,
    /// The host's configuration is wrong.
    Configuration,
    /// A fault, including an undeclared refusal code.
    Internal,
}

impl Category {
    /// The command-line exit code for this category.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::InvalidInput => 65,
            Self::NotFound => 66,
            Self::Conflict => 67,
            Self::Refused => 77,
            Self::Unavailable => 69,
            Self::Interrupted => 75,
            Self::Configuration => 78,
            Self::Internal => 70,
        }
    }
}

/// A refusal whose contributing operation's code is carried unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Refusal {
    /// The contributor's code, without renaming, prefixing or normalisation.
    pub code: String,
    /// The category declared for this code by the operation.
    pub category: Category,
    /// A description of why the call was refused.
    pub message: String,
    /// Structured evidence, including what a caller needs to resolve a conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Whether the caller may retry the call.
    pub retryable: bool,
    /// What is known about whether the operation had its effect.
    pub evidence: Evidence,
}

impl Refusal {
    /// A completed handler refusal with no details, initially marked as not retryable.
    /// Dispatchers set pre-handler refusals to [`Evidence::NotDispatched`].
    pub fn new(code: impl Into<String>, category: Category, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            category,
            message: message.into(),
            details: None,
            retryable: false,
            evidence: Evidence::Completed,
        }
    }

    /// Carry explicit dispatch evidence, including uncertainty from a remote reply loss.
    pub fn with_evidence(mut self, evidence: Evidence) -> Self {
        self.evidence = evidence;
        self
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Refusal {}
