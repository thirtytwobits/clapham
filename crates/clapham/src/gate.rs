use std::fmt;
use std::sync::Arc;

use crate::{ApplicationInfo, OAuthContext, Operation};

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

/// Host policy queried for call admission and catalogue visibility.
///
/// Calls consult the gate after input validation. `clapham.describe` also consults
/// it for each selected candidate, without dispatching that operation. Checks may
/// repeat; implementations must not create approval requests, consume approvals or
/// change authorisation state as a side effect. The host handles approval workflows
/// after a call returns `admission_pending`; a hidden catalogue candidate is not a
/// call attempt.
pub trait Gate: Send + Sync {
    /// Query whether this auth may run or discover this operation.
    /// This check must be safe to repeat without any subsequent dispatch.
    fn admit(&self, auth: &OAuthContext, operation: &Operation) -> Admission;
}

impl<F> Gate for F
where
    F: Fn(&OAuthContext, &Operation) -> Admission + Send + Sync,
{
    fn admit(&self, auth: &OAuthContext, operation: &Operation) -> Admission {
        self(auth, operation)
    }
}

/// Admit grants containing every scope required by the descriptor.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScopeGate;

impl Gate for ScopeGate {
    fn admit(&self, auth: &OAuthContext, operation: &Operation) -> Admission {
        if operation.scopes.iter().all(|scope| auth.has_scope(scope)) {
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
    /// Per-call admission policy. Defaults to [`ScopeGate`].
    pub gate: Arc<dyn Gate>,
    /// Whether daemon-local operations can run here.
    pub execution: ExecutionMode,
    /// The application's name and version, reported by `clapham.version` when supplied.
    pub application: Option<ApplicationInfo>,
    /// Expected OAuth issuer and audience, where bound by composition.
    pub oauth: Option<crate::OAuthResource>,
    /// Named application resource policies resolved by the shared composer.
    pub policies: std::collections::BTreeMap<String, crate::ResourcePolicy>,
}

impl Default for ComposeOptions {
    fn default() -> Self {
        Self {
            gate: Arc::new(ScopeGate),
            execution: ExecutionMode::InProcess,
            application: None,
            oauth: None,
            policies: Default::default(),
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
