use std::{future::Future, pin::Pin, sync::Arc};

use clapham::{
    Bounds, Call, Cancellation, Category, Contribution, Evidence, Operation, Outcome, RecordSink,
    Refusal, compose,
};
use clapham_mcp_client::{McpConfig, McpConnection, McpHttpOptions};
use serde_json::{Map, Value, json};

use crate::{Cli, Invocation};

/// A server response future, independent of the server's transport.
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Refusal>> + Send + 'a>>;

/// An authenticated connection to the operation service that owns execution.
pub trait Backend: Send + Sync + std::fmt::Debug {
    /// Discover the operations visible to this connection's OAuth grant.
    fn catalogue(&self) -> BackendFuture<'_, Vec<Operation>>;
    /// Invoke without accepting caller-supplied identity or scopes.
    fn call(
        &self,
        invocation: Invocation,
        records: RecordSink,
        cancel: Cancellation,
    ) -> BackendFuture<'_, clapham::Outcome>;
}

impl Cli {
    /// Build the CLI from an authenticated server's catalogue.
    pub async fn connect(
        name: impl Into<String>,
        backend: impl Backend + 'static,
        bounds: Bounds,
    ) -> Result<Self, Refusal> {
        let catalogue = backend.catalogue().await?;
        let names = catalogue.iter().map(|op| op.name.to_string()).collect();
        let mut contributions = std::collections::BTreeMap::new();
        for operation in catalogue {
            if operation.name.namespace() == Some("clapham") {
                continue;
            }
            let namespace = operation
                .name
                .namespace()
                .ok_or_else(|| crate::configuration("catalogue operation lacks a namespace"))?
                .to_owned();
            let contribution = contributions
                .entry(namespace.clone())
                .or_insert_with(|| Contribution::new(namespace));
            contribution.operations.push((
                operation,
                Box::new(|_: Call| async {
                    Err(Refusal::not_dispatched(
                        "no_backend",
                        Category::Configuration,
                        "execution belongs to the authenticated server",
                    ))
                }),
            ));
        }
        // Presentation schemas are validated locally. Authority and resource
        // policies are enforced by the service, never reconstructed from metadata.
        for contribution in contributions.values_mut() {
            for (operation, _) in &mut contribution.operations {
                operation.policies.clear();
            }
        }
        let set = compose(contributions.into_values(), bounds)
            .map_err(|e| crate::configuration(e.to_string()))?;
        let mut cli = Self::build(name.into(), set, None, Some(names))?;
        cli.backend = Some(Arc::new(backend));
        Ok(cli)
    }
}

/// The request `_meta` key that selects a dry run on Clapham's MCP server.
const PREVIEW_META_KEY: &str = "io.github.thirtytwobits.clapham/preview";

/// The daemon's MCP server, reached through Clapham's MCP client.
///
/// The catalogue is `clapham.describe`. A call is `tools/call`, with the operation's input as
/// its arguments and preview in request `_meta`. Records arrive under the records extension
/// and are acknowledged after the sink accepts each one. A refusal is the tool result's
/// structured refusal. Cancelling a call closes its response stream.
#[derive(Clone)]
pub struct McpBackend {
    connection: McpConnection,
}

impl std::fmt::Debug for McpBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpBackend").finish_non_exhaustive()
    }
}

impl McpBackend {
    /// Use a connection the host established, normally configured by [`McpConfig::operator`].
    pub fn new(connection: McpConnection) -> Self {
        Self { connection }
    }

    /// Connect to the daemon's Streamable HTTP endpoint with a token issued for its audience.
    pub async fn http(
        endpoint: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self, Refusal> {
        let mut options = McpHttpOptions::new(endpoint);
        options.bearer_token = Some(token.into());
        McpConnection::http(options, operator())
            .await
            .map(Self::new)
            .map_err(|error| error.refusal(Evidence::NotDispatched))
    }

    /// Connect over a launched daemon's standard streams, whose authority its host bound.
    pub async fn stdio<R, W>(reader: R, writer: W) -> Result<Self, Refusal>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        McpConnection::stdio(reader, writer, Default::default(), operator())
            .await
            .map(Self::new)
            .map_err(|error| error.refusal(Evidence::NotDispatched))
    }
}

fn operator() -> McpConfig {
    McpConfig::operator("daemon", "operator")
}

/// A tool result that carries a refusal becomes that refusal.
fn refused(outcome: Outcome) -> Result<Outcome, Refusal> {
    if outcome.is_error
        && let Some(refusal) = outcome
            .result
            .clone()
            .and_then(|value| serde_json::from_value::<Refusal>(value).ok())
    {
        return Err(refusal);
    }
    Ok(outcome)
}

impl Backend for McpBackend {
    fn catalogue(&self) -> BackendFuture<'_, Vec<Operation>> {
        Box::pin(async move {
            let outcome = self
                .connection
                .call_tool(
                    "clapham.describe",
                    Map::new(),
                    Map::new(),
                    RecordSink::default(),
                )
                .await
                .and_then(refused)?;
            let catalogue = outcome
                .result
                .ok_or_else(|| crate::configuration("clapham.describe returned no catalogue"))?;
            serde_json::from_value(catalogue).map_err(|e| crate::configuration(e.to_string()))
        })
    }
    fn call(
        &self,
        invocation: Invocation,
        records: RecordSink,
        cancel: Cancellation,
    ) -> BackendFuture<'_, Outcome> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(Refusal::not_dispatched(
                    "cancelled",
                    Category::Interrupted,
                    "cancelled before dispatch",
                ));
            }
            let Value::Object(arguments) = invocation.input else {
                return Err(crate::usage("operation input must be a JSON object"));
            };
            let mut meta = Map::new();
            if invocation.preview {
                meta.insert(PREVIEW_META_KEY.into(), json!(true));
            }
            let call = self
                .connection
                .call_tool(&invocation.name, arguments, meta, records);
            // Dropping the call cancels it.
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(Refusal::new(
                    "cancelled",
                    Category::Interrupted,
                    "the request was cancelled",
                )
                .with_evidence(Evidence::Uncertain)),
                outcome = call => outcome.and_then(refused),
            }
        })
    }
}
