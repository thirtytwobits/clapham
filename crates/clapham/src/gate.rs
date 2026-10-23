use std::fmt;
use std::sync::Arc;

use crate::{ApplicationInfo, Operation, Principal};

/// The gate's per-call admission decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The operation may run.
    Allow,
    /// The operation must not run.
    Deny,
    /// Approval is pending; no dispatch has occurred.
    Pending,
}

/// Host policy, consulted after input validation and before every dispatch.
pub trait Gate: Send + Sync {
    /// Decide whether this principal may run this operation.
    fn admit(&self, principal: &Principal, operation: &Operation) -> Admission;
}

impl<F> Gate for F
where
    F: Fn(&Principal, &Operation) -> Admission + Send + Sync,
{
    fn admit(&self, principal: &Principal, operation: &Operation) -> Admission {
        self(principal, operation)
    }
}

/// Admit precisely the principal kinds recorded in the descriptor.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultGate;

impl Gate for DefaultGate {
    fn admit(&self, principal: &Principal, operation: &Operation) -> Admission {
        if operation.principals.contains(&principal.kind) {
            Admission::Allow
        } else {
            Admission::Deny
        }
    }
}

/// The process in which the composed set runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Workspace operations run locally; daemon operations are unavailable.
    #[default]
    InProcess,
    /// The running daemon can serve both workspace and daemon operations.
    Daemon,
}

/// Host policy, execution mode and application identity, fixed at composition.
#[derive(Clone)]
pub struct ComposeOptions {
    /// Per-call admission policy. Defaults to [`DefaultGate`].
    pub gate: Arc<dyn Gate>,
    /// Whether daemon-local operations can run here.
    pub execution: ExecutionMode,
    /// The application's name and version, reported by `clapham.version` when supplied.
    pub application: Option<ApplicationInfo>,
}

impl Default for ComposeOptions {
    fn default() -> Self {
        Self {
            gate: Arc::new(DefaultGate),
            execution: ExecutionMode::InProcess,
            application: None,
        }
    }
}

impl fmt::Debug for ComposeOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ComposeOptions")
            .field("execution", &self.execution)
            .field("application", &self.application)
            .finish_non_exhaustive()
    }
}
