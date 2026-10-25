//! Copyright (c) Scott A Dixon
//! Connection lifecycle, admission limits and dispatch certainty.
use crate::{
    McpCatalogue,
    binding::Published,
    records::{RECORD_NOTIFICATION, Routes},
};
use rmcp::{
    ClientHandler, RoleClient,
    model::*,
    service::{NotificationContext, RequestContext, RunningService, ServiceError},
};
#[cfg(any(feature = "stdio", feature = "http"))]
use rmcp::{service::ClientLifecycleMode, transport::Transport};
#[cfg(any(feature = "stdio", feature = "http"))]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    sync::{Arc, Mutex, atomic::AtomicU64},
    time::Duration,
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

pub(crate) const REQUEST_ENVELOPE_RESERVE: usize = 4096;

/// Host authority and operation deadline. Reconnect with a new configuration after credentials change.
#[derive(Clone, Debug)]
pub struct McpConfig {
    /// Stable host identity and contribution namespace (ASCII letters, digits, underscore or hyphen).
    pub server_id: String,
    /// Non-secret revision fencing the authority of this connection.
    pub credential_scope_revision: String,
    /// Deadline covering queueing and one discovery or invocation, at most one hour.
    pub operation_timeout: Duration,
    /// Finite shared message, catalogue, queue and lifetime bounds.
    pub bounds: clapham::Bounds,
}
impl McpConfig {
    /// Configure a connection with default bounds and a thirty-second operation deadline.
    pub fn new(server_id: impl Into<String>, credential_scope_revision: impl Into<String>) -> Self {
        Self {
            server_id: server_id.into(),
            credential_scope_revision: credential_scope_revision.into(),
            operation_timeout: Duration::from_secs(30),
            bounds: clapham::Bounds::default(),
        }
    }
    /// Configure an operator's connection, such as a command line against its daemon: the
    /// largest message `Bounds` allows and one call at a time. The deadline covers
    /// connecting and discovery; [`McpConnection::call_tool`] calls run until the caller
    /// stops them.
    pub fn operator(
        server_id: impl Into<String>,
        credential_scope_revision: impl Into<String>,
    ) -> Self {
        let mut config = Self::new(server_id, credential_scope_revision);
        config.bounds = clapham::Bounds {
            message_bytes: 8 * 1024 * 1024,
            in_flight: 1,
            waiting: 0,
            ..clapham::Bounds::default()
        };
        config
    }
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub(crate) fn validate(&self) -> Result<(), McpError> {
        if self.server_id == "clapham"
            || self.server_id.is_empty()
            || self.server_id.len() > 128
            || !self
                .server_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.credential_scope_revision.trim().is_empty()
            || self.credential_scope_revision.len() > 256
            || self.operation_timeout.is_zero()
            || self.operation_timeout > Duration::from_secs(3600)
            || self.bounds.validate().is_err()
        {
            return Err(McpError::Configuration);
        }
        Ok(())
    }
}

/// Bounded diagnostics. Remote messages, credentials and response data are excluded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    /// Host configuration is outside the supported ranges.
    #[error("Invalid MCP configuration")]
    Configuration,
    /// A finite resource budget was exhausted.
    #[error("MCP resource limit exceeded")]
    Limit,
    /// Queueing or invocation exceeded its deadline.
    #[error("MCP operation deadline exceeded")]
    Timeout,
    /// The transport closed or failed.
    #[error("MCP transport closed or failed")]
    Transport,
    /// A response is invalid or unsupported.
    #[error("Invalid or unsupported MCP response")]
    Protocol,
    /// A JSON-RPC invocation error, retaining only its numeric code.
    #[error("MCP protocol invocation error ({0})")]
    Invocation(i32),
    /// The pinned catalogue no longer matches fresh discovery.
    #[error("MCP catalogue or selected definition changed")]
    Stale,
    /// A selected name or alias is invalid or repeated.
    #[error("MCP tool selection or alias is invalid or repeated")]
    Selection,
    /// The bounded operation queue is full.
    #[error("MCP operation queue is full")]
    Busy,
    /// The server did not accept the connection's credential.
    #[error("MCP credential was not accepted")]
    Unauthenticated,
    /// The remote requested an unadvertised interaction or continuation.
    #[error("MCP interaction requires an unsupported host capability")]
    UnsupportedInteraction,
}
impl McpError {
    /// Convert a bounded adapter diagnostic to its declared refusal and dispatch evidence.
    /// Numeric JSON-RPC codes are retained in details; remote messages and data are excluded.
    pub fn refusal(&self, evidence: clapham::Evidence) -> clapham::Refusal {
        use clapham::Category::*;
        let (code, category) = match self {
            Self::Configuration => ("mcp_configuration", Configuration),
            Self::Limit => ("mcp_limit", Unavailable),
            Self::Timeout => ("mcp_timeout", Interrupted),
            Self::Transport => ("mcp_transport", Unavailable),
            Self::Protocol => ("mcp_protocol", Internal),
            Self::Invocation(-32602) => ("mcp_invalid_input", InvalidInput),
            Self::Invocation(-32601) => ("mcp_not_found", NotFound),
            Self::Invocation(_) => ("mcp_invocation", Internal),
            Self::Stale => ("mcp_stale", Conflict),
            Self::Selection => ("mcp_selection", Usage),
            Self::Busy => ("mcp_busy", Unavailable),
            Self::Unauthenticated => ("mcp_unauthenticated", Refused),
            Self::UnsupportedInteraction => ("mcp_unsupported_interaction", Internal),
        };
        let mut refusal =
            clapham::Refusal::new(code, category, self.to_string()).with_evidence(evidence);
        refusal.retryable = matches!(self, Self::Timeout);
        if let Self::Invocation(code) = self {
            refusal.details = Some(serde_json::json!({"json_rpc_code":code}));
        }
        refusal
    }
    pub(crate) fn from_service(error: ServiceError) -> Self {
        match error {
            ServiceError::McpError(e) => Self::Invocation(e.code.0),
            ServiceError::UnexpectedResponse => Self::Protocol,
            ServiceError::Timeout { .. } => Self::Timeout,
            _ => Self::Transport,
        }
    }
}

pub(crate) struct State {
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub requests: crate::wire::Requests,
    pub epoch: AtomicU64,
    pub fault: Mutex<Option<McpError>>,
    #[cfg(any(feature = "stdio", feature = "http"))]
    incoming: AtomicUsize,
    #[cfg(any(feature = "stdio", feature = "http"))]
    incoming_bytes: AtomicUsize,
    pub routes: Arc<Routes>,
    pub stop: CancellationToken,
}
impl State {
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: crate::wire::Requests::default(),
            epoch: AtomicU64::new(0),
            fault: Mutex::new(None),
            incoming: AtomicUsize::new(0),
            incoming_bytes: AtomicUsize::new(0),
            routes: Arc::default(),
            stop: CancellationToken::new(),
        })
    }
    pub fn fail(&self, error: McpError) {
        *self.fault.lock().unwrap() = Some(error);
        self.stop.cancel();
    }
    pub fn check(&self) -> Result<(), McpError> {
        if let Some(error) = self.fault.lock().unwrap().clone() {
            return Err(error);
        }
        if self.stop.is_cancelled() {
            return Err(McpError::Transport);
        }
        Ok(())
    }
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub fn charge_incoming_bytes(&self, bytes: usize) -> Result<(), McpError> {
        const BUDGET: usize = 64 * 1024 * 1024;
        if bytes > BUDGET || self.incoming_bytes.fetch_add(bytes, Ordering::SeqCst) > BUDGET - bytes
        {
            return Err(McpError::Limit);
        }
        Ok(())
    }
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub fn observe_bytes(
        &self,
        message: &ServerJsonRpcMessage,
        bytes: usize,
    ) -> Result<(), McpError> {
        if matches!(
            message,
            ServerJsonRpcMessage::Request(_) | ServerJsonRpcMessage::Notification(_)
        ) && !self.routes.paced(message)
        {
            self.charge_incoming_bytes(bytes)?;
        }
        Ok(())
    }
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub fn observe(&self, message: &ServerJsonRpcMessage, limit: usize) -> Result<(), McpError> {
        if matches!(
            message,
            ServerJsonRpcMessage::Request(_) | ServerJsonRpcMessage::Notification(_)
        ) && !self.routes.admit(message)
            && self.incoming.fetch_add(1, Ordering::SeqCst) >= limit
        {
            return Err(McpError::Limit);
        }
        // Invalidate before the service schedules its notification handler.
        if matches!(message, ServerJsonRpcMessage::Notification(n) if matches!(n.notification, ServerNotification::ToolListChangedNotification(_)))
        {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

pub(crate) struct Handler {
    routes: Arc<Routes>,
}
#[allow(deprecated)]
impl ClientHandler for Handler {
    async fn list_roots(
        &self,
        _: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Err(ErrorData::method_not_found::<ListRootsRequestMethod>())
    }
    async fn create_elicitation(
        &self,
        _: ElicitRequestParams,
        _: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            "Unsupported elicitation",
            None,
        ))
    }
    async fn on_tool_list_changed(&self, _: NotificationContext<RoleClient>) {}
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleClient>,
    ) {
        if notification.method == RECORD_NOTIFICATION {
            self.routes.deliver(notification);
        }
    }
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            crate::records::capabilities(),
            Implementation::new("clapham-mcp-client", env!("CARGO_PKG_VERSION")),
        )
    }
}

pub(crate) struct Inner {
    pub service: RunningService<RoleClient, Handler>,
    pub config: McpConfig,
    pub state: Arc<State>,
    pub active: Semaphore,
    pub admitted: Semaphore,
    pub discovery: tokio::sync::Mutex<()>,
    pub published: Mutex<Option<Published>>,
    pub revision: AtomicU64,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.state.stop.cancel();
    }
}

/// A connection shared by pinned bindings. The host owns endpoint and process authority.
#[derive(Clone)]
pub struct McpConnection(pub(crate) Arc<Inner>);
impl McpConnection {
    #[cfg(any(feature = "stdio", feature = "http"))]
    pub(crate) async fn start<T: Transport<RoleClient> + 'static>(
        transport: T,
        config: McpConfig,
        state: Arc<State>,
    ) -> Result<Self, McpError> {
        let service = tokio::time::timeout(
            config.operation_timeout,
            rmcp::service::serve_client_with_lifecycle_and_ct(
                Handler {
                    routes: state.routes.clone(),
                },
                transport,
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
                state.stop.clone(),
            ),
        )
        .await;
        let service = match service {
            Ok(Ok(service)) => service,
            result => {
                let error = state.check().err().unwrap_or(match result {
                    Err(_) => McpError::Timeout,
                    Ok(Err(
                        rmcp::service::ClientInitializeError::TransportError { .. }
                        | rmcp::service::ClientInitializeError::ConnectionClosed(_)
                        | rmcp::service::ClientInitializeError::Cancelled,
                    )) => McpError::Transport,
                    _ => McpError::Protocol,
                });
                state.fail(error.clone());
                return Err(error);
            }
        };
        Ok(Self(Arc::new(Inner {
            active: Semaphore::new(config.bounds.in_flight),
            admitted: Semaphore::new(config.bounds.in_flight + config.bounds.waiting),
            config,
            service,
            state,
            discovery: tokio::sync::Mutex::new(()),
            published: Mutex::new(None),
            revision: AtomicU64::new(0),
        })))
    }
    /// Stop transport work. Process termination remains the host's responsibility.
    pub fn close(&self) {
        self.0.state.stop.cancel();
    }
    /// The stable host identity used as the contribution namespace.
    pub fn server_id(&self) -> &str {
        &self.0.config.server_id
    }
    /// The non-secret authority fence fixed at connection creation.
    pub fn credential_scope_revision(&self) -> &str {
        &self.0.config.credential_scope_revision
    }
    /// Discover an uncached, bounded catalogue. Failure invalidates prior bindings.
    pub async fn discover(&self) -> Result<McpCatalogue, McpError> {
        McpCatalogue::discover(self.clone()).await
    }
    pub(crate) fn check(&self) -> Result<(), McpError> {
        self.0.state.check()?;
        if self.0.service.is_closed() {
            return Err(McpError::Transport);
        }
        Ok(())
    }
}
