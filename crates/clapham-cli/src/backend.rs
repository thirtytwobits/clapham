use std::{future::Future, pin::Pin, sync::Arc};

use clapham::{
    Bounds, Call, Cancellation, Category, Contribution, Operation, RecordSink, Refusal, compose,
};
use serde_json::{Value, json};

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

/// HTTP client for the authenticated operation/record interface.
///
/// The token is supplied by the host's OAuth client or credential store. This
/// client does not accept identity claims from command-line arguments.
#[derive(Clone)]
pub struct HttpBackend {
    endpoint: String,
    token: String,
    client: reqwest::Client,
    bounds: Bounds,
}

impl std::fmt::Debug for HttpBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpBackend")
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

impl HttpBackend {
    /// Connect using a token issued for this operation service's audience.
    pub fn new(
        endpoint: impl Into<String>,
        token: impl Into<String>,
        bounds: Bounds,
    ) -> Result<Self, Refusal> {
        let endpoint = endpoint.into();
        let url =
            reqwest::Url::parse(&endpoint).map_err(|e| crate::configuration(e.to_string()))?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err(crate::configuration("operation HTTP requires HTTPS"));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.query().is_some()
        {
            return Err(crate::configuration("invalid operation service endpoint"));
        }
        bounds
            .validate()
            .map_err(|e| crate::configuration(e.to_string()))?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|e| crate::configuration(e.to_string()))?;
        Ok(Self {
            endpoint: endpoint.trim_end_matches('/').to_owned(),
            token: token.into(),
            client,
            bounds,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Frame {
    Record { data: Value, ack: uuid::Uuid },
    Outcome { data: clapham::Outcome },
    Refusal { data: Refusal },
}

fn transport() -> Refusal {
    Refusal::new(
        "connection_lost",
        Category::Unavailable,
        "operation service delivery failed",
    )
    .with_evidence(clapham::Evidence::Uncertain)
}

async fn server_refusal(mut response: reqwest::Response, limit: usize) -> Refusal {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= limit => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => return serde_json::from_slice(&bytes).unwrap_or_else(|_| transport()),
            _ => return transport(),
        }
    }
}

impl Backend for HttpBackend {
    fn catalogue(&self) -> BackendFuture<'_, Vec<Operation>> {
        Box::pin(async move {
            let mut response = self
                .client
                .get(format!("{}/operations", self.endpoint))
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|_| transport().with_evidence(clapham::Evidence::NotDispatched))?;
            if !response.status().is_success() {
                return Err(server_refusal(response, self.bounds.message_bytes).await);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| transport())? {
                if bytes.len().saturating_add(chunk.len()) > self.bounds.catalogue_bytes {
                    return Err(crate::configuration(
                        "catalogue exceeds bounds.catalogue_bytes",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|e| crate::configuration(e.to_string()))
        })
    }
    fn call(
        &self,
        invocation: Invocation,
        records: RecordSink,
        cancel: Cancellation,
    ) -> BackendFuture<'_, clapham::Outcome> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(Refusal::not_dispatched(
                    "cancelled",
                    Category::Interrupted,
                    "cancelled before dispatch",
                ));
            }
            let request = json!({"name":invocation.name,"input":invocation.input,"preview":invocation.preview});
            let encoded = serde_json::to_vec(&request).expect("call serialises");
            if encoded.len() > self.bounds.message_bytes {
                return Err(crate::usage("call exceeds bounds.message_bytes"));
            }
            let run = async {
                let mut response = self
                    .client
                    .post(format!("{}/call", self.endpoint))
                    .bearer_auth(&self.token)
                    .header("content-type", "application/json")
                    .body(encoded)
                    .send()
                    .await
                    .map_err(|_| transport())?;
                if !response.status().is_success() {
                    return Err(server_refusal(response, self.bounds.message_bytes).await);
                }
                let mut pending = Vec::new();
                let mut terminal: Option<Result<clapham::Outcome, Refusal>> = None;
                while let Some(chunk) = response.chunk().await.map_err(|_| transport())? {
                    for byte in chunk {
                        if terminal.is_some() {
                            return Err(transport());
                        }
                        pending.push(byte);
                        if pending.len() > self.bounds.message_bytes {
                            return Err(transport());
                        }
                        if byte == b'\n' {
                            let frame: Frame =
                                serde_json::from_slice(&pending).map_err(|_| transport())?;
                            pending.clear();
                            match frame {
                                Frame::Record { data, ack } => {
                                    records.send(data).await?;
                                    let acknowledged = self
                                        .client
                                        .post(format!("{}/ack/{ack}", self.endpoint))
                                        .bearer_auth(&self.token)
                                        .body(Vec::new())
                                        .send()
                                        .await
                                        .map_err(|_| transport())?;
                                    if acknowledged.status() != reqwest::StatusCode::NO_CONTENT {
                                        return Err(server_refusal(
                                            acknowledged,
                                            self.bounds.message_bytes,
                                        )
                                        .await
                                        .with_evidence(clapham::Evidence::Uncertain));
                                    }
                                }
                                Frame::Outcome { data } => terminal = Some(Ok(data)),
                                Frame::Refusal { data } => terminal = Some(Err(data)),
                            }
                        }
                    }
                }
                if pending.is_empty() {
                    terminal.unwrap_or_else(|| Err(transport()))
                } else {
                    Err(transport())
                }
            };
            tokio::select! { biased;
                _ = cancel.cancelled() => Err(Refusal::new("cancelled",Category::Interrupted,"the request was cancelled").with_evidence(clapham::Evidence::Uncertain)),
                result = run => result,
            }
        })
    }
}
