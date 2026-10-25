//! Copyright (c) Scott A Dixon
//! Discovery, host auth assignment and immutable remote definitions.
use crate::{McpConnection, McpError, records, wire};
use clapham::{
    Call, Category, Contribution, Effect, Evidence, Handler, HandlerFuture, Locality, Operation,
    OperationName, Outcome, Refusal,
};
use rmcp::model::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, atomic::Ordering},
};
use tokio::time::{Instant, timeout_at};

pub(crate) struct Published {
    epoch: u64,
    records: BTreeMap<String, Value>,
}

/// A refused catalogue entry. Messages contain no remote schema or metadata values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpDiagnostic {
    /// Zero-based record index across all pages.
    pub index: usize,
    /// The remote name, if the record contains a string name.
    pub name: Option<String>,
    /// Bounded reason for refusing this record.
    pub error: McpError,
}

/// Bounded discovery snapshot. Original records are available explicitly as host provenance.
pub struct McpCatalogue {
    connection: McpConnection,
    revision: u64,
    epoch: u64,
    records: BTreeMap<String, Value>,
    admitted: BTreeMap<String, Tool>,
    diagnostics: Vec<McpDiagnostic>,
}
impl McpCatalogue {
    /// Monotonic number for successful discoveries on this connection.
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Refused records in their original catalogue order.
    pub fn diagnostics(&self) -> &[McpDiagnostic] {
        &self.diagnostics
    }
    /// Valid, unique remote names in sorted order.
    pub fn remote_names(&self) -> impl Iterator<Item = &str> {
        self.admitted.keys().map(String::as_str)
    }
    /// The original server record, including untrusted remote metadata.
    pub fn provenance(&self, name: &str) -> Option<&Value> {
        self.records.get(name)
    }
    pub(crate) async fn discover(connection: McpConnection) -> Result<Self, McpError> {
        connection.check()?;
        let deadline = Instant::now() + connection.0.config.operation_timeout;
        let _ticket = connection
            .0
            .admitted
            .try_acquire()
            .map_err(|_| McpError::Busy)?;
        let mut cancel_on_drop = None;
        let work = async {
            let _serial = connection.0.discovery.lock().await;
            let _active = connection
                .0
                .active
                .acquire()
                .await
                .map_err(|_| McpError::Transport)?;
            // Raw catalogue requests bypass the SDK cache. Close on abandonment so their
            // request state cannot outlive the operation and accumulate across cancelled refreshes.
            cancel_on_drop = Some(connection.0.state.stop.clone().drop_guard());
            // Invalidate before I/O. Any failed refresh leaves prior bindings stale.
            let epoch = connection.0.state.epoch.fetch_add(1, Ordering::SeqCst) + 1;
            let limits = &connection.0.config.bounds;
            let mut cursor = None;
            let mut cursors = BTreeSet::new();
            let mut records = BTreeMap::new();
            let mut admitted = BTreeMap::new();
            let mut first_indices = BTreeMap::new();
            let mut diagnostics = Vec::new();
            let mut bytes = 0usize;
            let mut count = 0;
            for page in 0..limits.pages {
                let request = serde_json::from_value(
                    json!({"method":"tools/list","params":{"cursor":cursor}}),
                )
                .map_err(|_| McpError::Protocol)?;
                // Avoid the SDK's TTL cache and stale-on-error fallback during refresh.
                let response = connection
                    .0
                    .service
                    .peer()
                    .send_request(request)
                    .await
                    .map_err(McpError::from_service)?;
                let value = serde_json::to_value(response).map_err(|_| McpError::Protocol)?;
                let value = value.get("claphamCatalogue").ok_or(McpError::Protocol)?;
                bytes = bytes
                    .checked_add(wire::encode(value, limits.catalogue_bytes)?.len())
                    .ok_or(McpError::Limit)?;
                if bytes > limits.catalogue_bytes {
                    return Err(McpError::Limit);
                }
                let tools = value
                    .get("tools")
                    .and_then(Value::as_array)
                    .ok_or(McpError::Protocol)?;
                for raw in tools {
                    count += 1;
                    if count > limits.tools {
                        return Err(McpError::Limit);
                    }
                    let name = raw.get("name").and_then(Value::as_str).map(str::to_owned);
                    let admission = (|| {
                        let name = name.as_ref().ok_or(McpError::Selection)?;
                        if records.insert(name.clone(), raw.clone()).is_some() {
                            if admitted.remove(name).is_some() {
                                diagnostics.push(McpDiagnostic {
                                    index: first_indices[name],
                                    name: Some(name.clone()),
                                    error: McpError::Selection,
                                });
                            }
                            return Err(McpError::Selection);
                        }
                        first_indices.insert(name.clone(), count - 1);
                        let tool: Tool =
                            serde_json::from_value(raw.clone()).map_err(|_| McpError::Selection)?;
                        if OperationName::new(name).is_err()
                            || tool
                                .description
                                .as_deref()
                                .is_none_or(|description| description.trim().is_empty())
                        {
                            return Err(McpError::Selection);
                        }
                        wire::encode(&tool.input_schema, limits.schema_bytes)?;
                        if let Some(schema) = &tool.output_schema {
                            wire::encode(schema, limits.schema_bytes)?;
                        }
                        if tool.input_schema.get("type").and_then(Value::as_str) != Some("object") {
                            return Err(McpError::Selection);
                        }
                        jsonschema::options()
                            .offline()
                            .build(&Value::Object((*tool.input_schema).clone()))
                            .map_err(|_| McpError::Selection)?;
                        if let Some(schema) = &tool.output_schema {
                            jsonschema::options()
                                .offline()
                                .build(&Value::Object((**schema).clone()))
                                .map_err(|_| McpError::Selection)?;
                        }
                        admitted.insert(name.clone(), tool);
                        Ok(())
                    })();
                    if let Err(error) = admission {
                        diagnostics.push(McpDiagnostic {
                            index: count - 1,
                            name,
                            error,
                        });
                    }
                }
                cursor = match value.get("nextCursor") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(cursor)) => Some(cursor.clone()),
                    _ => return Err(McpError::Protocol),
                };
                if let Some(cursor) = &cursor {
                    if !cursors.insert(cursor.clone()) || page + 1 == limits.pages {
                        return Err(McpError::Limit);
                    }
                } else {
                    break;
                }
            }
            if epoch != connection.0.state.epoch.load(Ordering::SeqCst) {
                return Err(McpError::Stale);
            }
            *connection.0.published.lock().unwrap() = Some(Published {
                epoch,
                records: records
                    .iter()
                    .filter(|(name, _)| admitted.contains_key(*name))
                    .map(|(name, record)| (name.clone(), record.clone()))
                    .collect(),
            });
            diagnostics.sort_by_key(|diagnostic| diagnostic.index);
            let revision = connection.0.revision.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Self {
                connection: connection.clone(),
                revision,
                epoch,
                records,
                admitted,
                diagnostics,
            })
        };
        let result = timeout_at(deadline, work).await;
        if let Some(guard) = cancel_on_drop {
            guard.disarm();
        }
        match result {
            Ok(result) => result,
            Err(_) => {
                connection.0.state.fail(McpError::Timeout);
                Err(McpError::Timeout)
            }
        }
    }
    /// Pin selected records as operations in the connection's namespace.
    /// Aliases are local names; composition checks collisions with other contributions atomically.
    pub fn bind(&self, selections: Vec<McpSelection>) -> Result<McpToolBinding, McpError> {
        self.connection.check()?;
        if self.epoch != self.connection.0.state.epoch.load(Ordering::SeqCst) {
            return Err(McpError::Stale);
        }
        if selections.is_empty() || selections.len() > self.connection.0.config.bounds.tools {
            return Err(McpError::Selection);
        }
        let mut operations = Vec::new();
        let mut aliases = BTreeMap::new();
        let mut pinned = BTreeMap::new();
        for selection in selections {
            let tool = self
                .admitted
                .get(&selection.remote_name)
                .ok_or(McpError::Selection)?;
            let local = selection.alias.as_deref().unwrap_or(&selection.remote_name);
            if local.is_empty() {
                return Err(McpError::Selection);
            }
            let name = OperationName::new(format!("{}.{}", self.connection.server_id(), local))
                .map_err(|_| McpError::Selection)?;
            if aliases
                .insert(name.to_string(), selection.remote_name.clone())
                .is_some()
                || pinned
                    .insert(
                        selection.remote_name.clone(),
                        self.records[&selection.remote_name].clone(),
                    )
                    .is_some()
            {
                return Err(McpError::Selection);
            }
            let hints = tool.annotations.as_ref();
            // A remote administrative claim can restrict access, never grant that class.
            let administrative = tool
                .meta
                .as_ref()
                .and_then(|meta| meta.get("io.github.thirtytwobits.clapham/effect"))
                .and_then(Value::as_str)
                == Some("administrative");
            let effect = if administrative {
                Effect::Destructive
            } else if hints.and_then(|h| h.read_only_hint).unwrap_or(false) {
                Effect::Query
            } else if hints.and_then(|h| h.destructive_hint).unwrap_or(true) {
                Effect::Destructive
            } else {
                Effect::Mutation
            };
            operations.push(Operation {
                name,
                title: tool.title.clone(),
                description: tool
                    .description
                    .as_deref()
                    .expect("admitted description")
                    .to_owned(),
                input: (*tool.input_schema).clone(),
                output: tool.output_schema.as_ref().map(|s| {
                    Value::Object((**s).clone())
                        .try_into()
                        .expect("object schema")
                }),
                effect,
                scopes: selection.scopes,
                policies: Vec::new(),
                confirmation: false,
                locality: Locality::Daemon,
                refusals: remote_refusals(),
                streams: tool
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get(records::RECORDS_EXTENSION))
                    == Some(&Value::Bool(true)),
                preview: false,
                positional: Vec::new(),
                idempotent: hints.and_then(|h| h.idempotent_hint).unwrap_or(false),
                open_world: hints.and_then(|h| h.open_world_hint).unwrap_or(true),
            });
        }
        let remote = Arc::new(Remote {
            connection: self.connection.clone(),
            aliases,
            pinned,
        });
        let revision = binding_digest(json!({
            "server": self.connection.server_id(),
            "credentialScope": self.connection.credential_scope_revision(),
            "definitions": remote.pinned,
            "aliases": remote.aliases,
            "operations": operations,
        }));
        Ok(McpToolBinding {
            revision,
            remote,
            operations,
        })
    }
}

/// Host-selected remote tool and optional local alias and OAuth scopes.
#[derive(Clone, Debug)]
pub struct McpSelection {
    /// The exact name advertised by the remote server.
    pub remote_name: String,
    /// A local name within the connection's namespace; defaults to the remote name.
    pub alias: Option<String>,
    /// Host-authorised OAuth scopes. All listed scopes are required.
    pub scopes: Vec<String>,
}
impl McpSelection {
    /// Select a remote name with its original spelling and host-specified OAuth scopes.
    pub fn new(remote_name: impl Into<String>) -> Self {
        Self {
            remote_name: remote_name.into(),
            alias: None,
            scopes: Vec::new(),
        }
    }
}

/// An accepted catalogue revision and immutable aliases, definitions and credential scope.
pub struct McpToolBinding {
    revision: String,
    remote: Arc<Remote>,
    operations: Vec<Operation>,
}
impl McpToolBinding {
    /// Digest of the server, credential scope, pinned records, aliases and descriptors.
    pub fn revision(&self) -> &str {
        &self.revision
    }
    /// The host's non-secret credential scope fence.
    pub fn credential_scope_revision(&self) -> &str {
        self.remote.connection.credential_scope_revision()
    }
    /// Descriptors to be composed, in selection order.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
    /// Create a contribution retaining this connection and binding revision.
    pub fn contribution(&self) -> Contribution {
        let mut contribution = Contribution::new(self.remote.connection.server_id())
            .with_binding_revision(self.revision.clone());
        for operation in &self.operations {
            let remote = self.remote.clone();
            contribution = contribution.operation(operation.clone(), move |call| {
                let remote = remote.clone();
                async move { remote.call(call).await }
            });
        }
        contribution
    }
}

fn remote_refusals() -> BTreeMap<String, Category> {
    [
        McpError::Configuration,
        McpError::Limit,
        McpError::Timeout,
        McpError::Transport,
        McpError::Protocol,
        McpError::Invocation(-32602),
        McpError::Invocation(-32601),
        McpError::Invocation(-32603),
        McpError::Stale,
        McpError::Selection,
        McpError::Busy,
        McpError::Unauthenticated,
        McpError::UnsupportedInteraction,
    ]
    .into_iter()
    .map(|error| {
        let refusal = error.refusal(Evidence::NotDispatched);
        (refusal.code, refusal.category)
    })
    .collect()
}

fn binding_digest(value: Value) -> String {
    use sha2::{Digest, Sha256};
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"clapham.mcp-binding.v1\0");
    hash.update(canonical(value).to_string().as_bytes());
    format!("sha256:{:x}", hash.finalize())
}

struct Remote {
    connection: McpConnection,
    aliases: BTreeMap<String, String>,
    pinned: BTreeMap<String, Value>,
}
impl Remote {
    fn check(&self) -> Result<(), McpError> {
        self.connection.check()?;
        let published = self.connection.0.published.lock().unwrap();
        let published = published.as_ref().ok_or(McpError::Stale)?;
        if published.epoch != self.connection.0.state.epoch.load(Ordering::SeqCst)
            || self
                .pinned
                .iter()
                .any(|(name, record)| published.records.get(name) != Some(record))
        {
            return Err(McpError::Stale);
        }
        Ok(())
    }
}
impl Handler for Remote {
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        Box::pin(async move { self.invoke(call).await })
    }
}
impl Remote {
    async fn invoke(&self, call: Call) -> Result<Outcome, Refusal> {
        let deadline = Instant::now() + self.connection.0.config.operation_timeout;
        let refused = |error: McpError| error.refusal(Evidence::NotDispatched);
        self.check().map_err(refused)?;
        let name = self
            .aliases
            .get(call.name.as_str())
            .ok_or_else(|| refused(McpError::Selection))?;
        let arguments = call
            .input
            .as_object()
            .cloned()
            .ok_or_else(|| refused(McpError::Selection))?;
        self.connection
            .invoke(
                name,
                arguments,
                JsonObject::new(),
                &call.records,
                Some(deadline),
                || self.check(),
            )
            .await
    }
}
