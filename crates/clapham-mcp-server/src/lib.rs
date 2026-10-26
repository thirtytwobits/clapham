#![doc = include_str!("../README.md")]

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::{AuthenticationFuture, Authenticator, Credential, HttpServer};
#[cfg(feature = "http")]
mod oauth;
#[cfg(feature = "http")]
pub use oauth::OAuthIntrospector;

mod mapping;
mod records;
pub use records::{ACKNOWLEDGE_REQUEST, RECORD_NOTIFICATION, RECORDS_EXTENSION};
mod resource;
#[cfg(all(test, feature = "stdio", feature = "http"))]
mod rmcp_facts;
#[cfg(feature = "stdio")]
mod stdio;

use std::{borrow::Cow, sync::Arc};

use clapham::{
    Cancellation, Category, ComposedSet, OAuthContext, Operation, RecordSink, Refusal, Selection,
};
use rmcp::{
    ErrorData, ServerHandler,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// MCP tool metadata identifying an administrative operation.
pub const EFFECT_META_KEY: &str = "io.github.thirtytwobits.clapham/effect";

/// Request metadata key selecting a dry run.
pub const PREVIEW_META_KEY: &str = "io.github.thirtytwobits.clapham/preview";

/// The catalogue extension. A request of the same name, with an optional `cursor`, returns
/// `{operations, nextCursor?}`: one page of the descriptors admitted to the caller from the
/// server's selection, read through `clapham.describe` whether or not the selection exposes it.
pub const CATALOGUE_EXTENSION: &str = "io.github.thirtytwobits.clapham/catalogue";

/// Published protocol, client identity, preview and records metadata keys used by the server.
/// The conformance wire record takes its request-key contract from this list.
pub const REQUEST_META_KEYS: &[&str] = &[
    PREVIEW_META_KEY,
    RECORDS_EXTENSION,
    "io.modelcontextprotocol/clientCapabilities",
    "io.modelcontextprotocol/clientInfo",
    "io.modelcontextprotocol/protocolVersion",
];

// Space for the JSON-RPC envelope, lifecycle metadata, cursor and newline. The
// stdio transport rechecks the exact encoded frame before writing any bytes.
const ENVELOPE_RESERVE: usize = 1024;

/// MCP 2026-07-28 over a selection, bound to verified OAuth authority supplied by the host.
///
/// Clones share admission and execution state. Identity never comes from MCP input.
#[derive(Debug, Clone)]
pub struct Server {
    set: ComposedSet,
    selection: Selection,
    discovery: Selection,
    auth: OAuthContext,
    resources: std::collections::BTreeMap<String, String>,
    slots: Option<Arc<tokio::sync::Semaphore>>,
    deliveries: Arc<records::Deliveries>,
    #[cfg(feature = "http")]
    http_failure: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Server {
    /// Expose all operations. Resource templates must use simple string variables
    /// (`lists://{id}`); unsupported forms and duplicate template shapes refuse configuration.
    pub fn new(set: ComposedSet, auth: OAuthContext) -> Result<Self, Refusal> {
        let selection = set.all();
        let slots = Arc::new(tokio::sync::Semaphore::new(set.bounds().in_flight));
        let server = Self {
            discovery: selection.clone(),
            selection,
            set,
            auth,
            resources: Default::default(),
            slots: Some(slots),
            deliveries: Default::default(),
            #[cfg(feature = "http")]
            http_failure: None,
        };
        server.validate_resources()?;
        Ok(server)
    }

    /// Register MCP resource URI templates against query operations.
    pub fn resources<I, N, U>(mut self, resources: I) -> Result<Self, Refusal>
    where
        I: IntoIterator<Item = (N, U)>,
        N: Into<String>,
        U: Into<String>,
    {
        register_resources(&self.set, &mut self.resources, resources)?;
        Ok(self)
    }

    /// Replace the exposed selection with complete operation names from this set.
    /// Discovery uses the built-in internally even when it is not itself exposed.
    pub fn select<I, S>(mut self, names: I) -> Result<Self, Refusal>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.selection = self.set.select(names)?;
        self.discovery = self.set.select(
            self.selection
                .operations()
                .map(|op| op.name.as_str())
                .chain(["clapham.describe"]),
        )?;
        self.validate_resources()?;
        Ok(self)
    }

    fn validate_resources(&self) -> Result<(), Refusal> {
        validate_resources(&self.selection, &self.resources)
    }

    // Named describes avoid forcing the complete catalogue into one core result.
    // The same gate filters discovery and dispatch, including custom admission policy.
    async fn catalogue(
        &self,
        include: impl Fn(&Operation) -> bool,
    ) -> Result<Vec<Operation>, ErrorData> {
        let mut operations = Vec::new();
        for op in self.selection.operations().filter(|op| include(op)) {
            let result = self
                .set
                .call(
                    &self.discovery,
                    self.auth.clone(),
                    "clapham.describe",
                    json!({"name":op.name}),
                    RecordSink::default(),
                    Cancellation::new(),
                    false,
                )
                .await;
            match result {
                Ok(outcome) => operations.push(
                    serde_json::from_value(outcome.result.expect("describe returns a result"))
                        .map_err(|_| ErrorData::internal_error("invalid description", None))?,
                ),
                Err(refusal) if refusal.code == "unknown_operation" => {}
                Err(refusal) => return Err(protocol_refusal(refusal)),
            }
        }
        Ok(operations)
    }

    /// Run one call. With a records token, records reach the client under the records
    /// extension; without one they are discarded and the client receives the result alone.
    async fn invoke(
        &self,
        name: &str,
        input: Value,
        preview: bool,
        token: Option<&str>,
        context: &RequestContext<RoleServer>,
    ) -> clapham::CallResult {
        let cancel = Cancellation::new();
        if context.ct.is_cancelled() {
            cancel.cancel();
        }
        let records = match token {
            // The newline after a stdio frame is outside the encoded notification.
            Some(token) => records::sink(
                self.deliveries.clone(),
                token.to_owned(),
                context.peer.clone(),
                cancel.clone(),
                self.set.bounds().message_bytes.saturating_sub(1),
            ),
            None => RecordSink::default(),
        };
        let call = self.set.call(
            &self.selection,
            self.auth.clone(),
            name,
            input,
            records,
            cancel.clone(),
            preview,
        );
        tokio::pin!(call);
        tokio::select! {
            biased;
            () = context.ct.cancelled() => { cancel.cancel(); call.await },
            result = &mut call => result,
        }
    }

    async fn execution_permit(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, ErrorData> {
        // HTTP already holds its whole-request permit. Stdio acquires one for
        // calls/resource reads; cancelled waiters reach core without dispatch.
        let Some(slots) = &self.slots else {
            return Ok(None);
        };
        tokio::select! {
            biased;
            () = context.ct.cancelled() => Ok(None),
            permit = slots.acquire() => permit.map(Some).map_err(|_| ErrorData::internal_error("server closed", None)),
        }
    }

    #[cfg(feature = "http")]
    fn reject_http_reply(&self) -> ErrorData {
        if let Some(failure) = &self.http_failure {
            failure.store(true, std::sync::atomic::Ordering::Release);
        }
        ErrorData::internal_error(
            "the encoded reply exceeds bounds.message_bytes; operation effects are uncertain",
            None,
        )
    }

    fn resource_reply_allowed<T: Serialize>(
        &self,
        value: &T,
        budget: usize,
        refusal: bool,
    ) -> Result<bool, ErrorData> {
        let fits = mapping::size(value, budget).is_some();
        #[cfg(feature = "http")]
        if self.http_failure.is_some() && refusal && !fits {
            return Err(self.reject_http_reply());
        }
        // Stdio passes existing refusals unchanged to its bounded transport;
        // only ordinary outcomes may be replaced with result_too_large.
        Ok(refusal || fits)
    }

    fn response_budget(&self, context: &RequestContext<RoleServer>) -> usize {
        let limit = self.set.bounds().message_bytes;
        let id_size = mapping::size(&context.id, limit).unwrap_or(limit);
        limit.saturating_sub(ENVELOPE_RESERVE + id_size)
    }

    fn page<T: Serialize + Clone>(
        &self,
        kind: &str,
        items: &[T],
        request: Option<PaginatedRequestParams>,
        context: &RequestContext<RoleServer>,
    ) -> Result<(Vec<T>, Option<String>), ErrorData> {
        let bounds = self.set.bounds();
        // Resources and templates are subsets of operations, so the same count
        // limit also bounds their lists.
        if items.len() > bounds.tools || mapping::size(items, bounds.catalogue_bytes).is_none() {
            return Err(ErrorData::internal_error(
                "catalogue exceeds connection bounds",
                None,
            ));
        }
        let revision = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(items).expect("MCP models serialize"))
        );
        let prefix = format!("{kind}:{revision}");
        let (start, page) = match request.and_then(|r| r.cursor) {
            None => (0, 1),
            Some(cursor) => {
                let tail = cursor
                    .strip_prefix(&format!("{prefix}:"))
                    .ok_or_else(|| ErrorData::invalid_params("invalid or stale cursor", None))?;
                let (offset, page) = tail
                    .split_once(':')
                    .ok_or_else(|| ErrorData::invalid_params("invalid cursor", None))?;
                let start = offset
                    .parse::<usize>()
                    .map_err(|_| ErrorData::invalid_params("invalid cursor", None))?;
                let page = page
                    .parse::<usize>()
                    .map_err(|_| ErrorData::invalid_params("invalid cursor", None))?;
                if start == 0 || start >= items.len() || !(2..=bounds.pages).contains(&page) {
                    return Err(ErrorData::invalid_params(
                        "cursor exceeds catalogue bounds",
                        None,
                    ));
                }
                (start, page)
            }
        };
        let budget = self.response_budget(context);
        let mut result = Vec::new();
        let mut used = 2;
        let mut end = start;
        for item in &items[start..] {
            let bytes = mapping::size(item, budget).ok_or_else(|| {
                ErrorData::internal_error("catalogue item exceeds message bounds", None)
            })? + 1;
            if used + bytes > budget {
                break;
            }
            used += bytes;
            result.push(item.clone());
            end += 1;
        }
        if end == start && start < items.len() {
            return Err(ErrorData::internal_error(
                "catalogue item exceeds message bounds",
                None,
            ));
        }
        let cursor = if end < items.len() {
            if page == bounds.pages {
                return Err(ErrorData::internal_error(
                    "catalogue exceeds page bounds",
                    None,
                ));
            }
            Some(format!("{prefix}:{end}:{}", page + 1))
        } else {
            None
        };
        Ok((result, cursor))
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        capabilities.extensions = Some(clapham_extensions());
        ServerConfig::new(capabilities)
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
            .with_server_info(Implementation::new("clapham", env!("CARGO_PKG_VERSION")))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    async fn discover(&self, _: RequestContext<RoleServer>) -> Result<DiscoverResult, ErrorData> {
        let operations = self.catalogue(|_| true).await?;
        let mut capabilities = ServerCapabilities::default();
        capabilities.extensions = Some(clapham_extensions());
        if !operations.is_empty() {
            capabilities.tools = self.get_info().capabilities.tools;
        }
        if operations
            .iter()
            .any(|op| self.resources.contains_key(op.name.as_str()))
        {
            capabilities.resources = self.get_info().capabilities.resources;
        }
        Ok(
            DiscoverResult::new(vec![ProtocolVersion::V_2026_07_28], capabilities)
                .with_server_info(self.get_info().server_info),
        )
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self
            .catalogue(|_| true)
            .await?
            .iter()
            .map(|op| mapping::tool(op, self.set.bounds().schema_bytes))
            .collect::<Result<Vec<_>, _>>()?;
        let (tools, cursor) = self.page("tools", &tools, request, &context)?;
        let mut result = ListToolsResult::with_all_items(tools)
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.next_cursor = cursor;
        Ok(result)
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        // rmcp uses this descriptor only to validate HTTP routing headers; actual
        // visibility and dispatch still go through describe and the call runner.
        self.selection
            .operations()
            .find(|op| op.name.as_str() == name)
            .and_then(|op| mapping::tool(op, self.set.bounds().schema_bytes).ok())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let preview = match context.meta.get(PREVIEW_META_KEY) {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => {
                return Err(ErrorData::invalid_params(
                    "preview metadata must be a boolean",
                    None,
                ));
            }
        };
        let token = records::token(&context)?;
        let registration = match &token {
            Some(token) => {
                Some(self.deliveries.register(token, &self.auth).ok_or_else(|| {
                    ErrorData::invalid_params("the records token is in use", None)
                })?)
            }
            None => None,
        };
        let permit = self.execution_permit(&context).await?;
        let outcome = self
            .invoke(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
                preview,
                token.as_deref(),
                &context,
            )
            .await;
        drop(permit);
        drop(registration);
        let result = match outcome {
            Ok(outcome) => mapping::outcome(outcome).unwrap_or_else(mapping::refusal),
            Err(refusal) => mapping::refusal(refusal),
        };
        #[cfg(feature = "http")]
        let result = if self.http_failure.is_some()
            && mapping::size(&result, self.response_budget(&context)).is_none()
        {
            if result.is_error == Some(true) {
                return Err(self.reject_http_reply());
            }
            let replacement = mapping::refusal(mapping::too_large());
            // Long request IDs can leave too little space even for a refusal.
            if mapping::size(&replacement, self.response_budget(&context)).is_none() {
                return Err(self.reject_http_reply());
            }
            replacement
        } else {
            result
        };
        Ok(result.into())
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        if request.method != CATALOGUE_EXTENSION {
            return records::acknowledge(&self.deliveries, &self.auth, request);
        }
        #[derive(serde::Deserialize)]
        struct Page {
            #[serde(default)]
            cursor: Option<String>,
        }
        let Ok(page) = request
            .params_as::<Page>()
            .map(|page| page.unwrap_or(Page { cursor: None }))
        else {
            return Err(ErrorData::invalid_params("invalid catalogue request", None));
        };
        let operations = self.catalogue(|_| true).await?;
        let request = PaginatedRequestParams::default().with_cursor(page.cursor);
        let (operations, cursor) = self.page("operations", &operations, Some(request), &context)?;
        let mut result = json!({"operations":operations});
        if let Some(cursor) = cursor {
            result["nextCursor"] = json!(cursor);
        }
        Ok(CustomResult::new(result))
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let templates = self
            .catalogue(|op| self.resources.contains_key(op.name.as_str()))
            .await?
            .iter()
            .filter_map(|op| {
                self.resources.get(op.name.as_str()).map(|uri| {
                    let mut template = ResourceTemplate::new(uri.clone(), op.name.to_string())
                        .with_description(op.description.clone())
                        .with_mime_type("application/json");
                    template.title = op.title.clone();
                    template
                })
            })
            .collect::<Vec<_>>();
        let (templates, cursor) = self.page("templates", &templates, request, &context)?;
        let mut result = ListResourceTemplatesResult::with_all_items(templates)
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.next_cursor = cursor;
        Ok(result)
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let resources = self
            .catalogue(|op| self.resources.contains_key(op.name.as_str()))
            .await?
            .iter()
            .filter_map(|op| {
                self.resources
                    .get(op.name.as_str())
                    .filter(|uri| !uri.contains('{'))
                    .map(|uri| {
                        let mut resource = Resource::new(uri.clone(), op.name.to_string())
                            .with_description(op.description.clone())
                            .with_mime_type("application/json");
                        resource.title = op.title.clone();
                        resource
                    })
            })
            .collect::<Vec<_>>();
        let (resources, cursor) = self.page("resources", &resources, request, &context)?;
        let mut result = ListResourcesResult::with_all_items(resources)
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.next_cursor = cursor;
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let operations = self
            .catalogue(|op| self.resources.contains_key(op.name.as_str()))
            .await?;
        let exact = operations.iter().any(|op| {
            self.resources.get(op.name.as_str()).map(String::as_str) == Some(request.uri.as_str())
        });
        let mut matched = None;
        for op in operations {
            if exact
                && self.resources.get(op.name.as_str()).map(String::as_str)
                    != Some(request.uri.as_str())
            {
                continue;
            }
            if self.resources.contains_key(op.name.as_str()) {
                let template = resource::Template::parse(&op, &self.resources[op.name.as_str()])
                    .map_err(protocol_refusal)?;
                if let Some(input) = template.bind(&request.uri) {
                    if matched.is_some() {
                        return Err(ErrorData::invalid_params("ambiguous resource URI", None));
                    }
                    matched = Some((op.name.to_string(), input));
                }
            }
        }
        let (name, input) = matched.ok_or_else(|| {
            ErrorData::invalid_params("resource not found", Some(json!({"uri":request.uri})))
        })?;
        let _permit = self.execution_permit(&context).await?;
        let outcome = self
            .invoke(&name, Value::Object(input), false, None, &context)
            .await;
        // Resource reads have no tool-error union. Preserve refusals as JSON, and
        // retain the full Outcome when a plain result would discard semantics.
        let (value, refusal) = match outcome {
            Ok(outcome)
                if outcome.is_error || outcome.content.is_some() || outcome.result.is_none() =>
            {
                (
                    serde_json::to_value(outcome).expect("outcomes serialize"),
                    false,
                )
            }
            Ok(outcome) => (outcome.result.expect("plain outcome has a result"), false),
            Err(refusal) => (
                serde_json::to_value(refusal).expect("refusals serialize"),
                true,
            ),
        };
        let budget = self.response_budget(&context);
        if self.resource_reply_allowed(&value, budget, refusal)? {
            let result = resource_result(value, &request.uri);
            if self.resource_reply_allowed(&result, budget, refusal)? {
                return Ok(result.into());
            }
        }
        Ok(resource_result(
            serde_json::to_value(mapping::too_large()).expect("refusals serialize"),
            &request.uri,
        )
        .into())
    }
}

fn clapham_extensions() -> ExtensionCapabilities {
    ExtensionCapabilities::from([
        (CATALOGUE_EXTENSION.to_owned(), JsonObject::new()),
        (RECORDS_EXTENSION.to_owned(), JsonObject::new()),
    ])
}

fn configuration(message: impl Into<String>) -> Refusal {
    Refusal::not_dispatched("mcp_configuration", Category::Configuration, message)
}

fn protocol_refusal(refusal: Refusal) -> ErrorData {
    ErrorData::internal_error(
        "cannot describe the admitted selection",
        Some(serde_json::to_value(refusal).expect("refusals serialize")),
    )
}

fn resource_result(value: Value, uri: &str) -> ReadResourceResult {
    ReadResourceResult::new(vec![
        ResourceContents::text(value.to_string(), uri).with_mime_type("application/json"),
    ])
    .with_ttl_ms(0)
    .with_cache_scope(CacheScope::Private)
}

fn validate_resources(
    selection: &Selection,
    resources: &std::collections::BTreeMap<String, String>,
) -> Result<(), Refusal> {
    let mut templates = std::collections::BTreeSet::new();
    for op in selection.operations() {
        if let Some(uri) = resources.get(op.name.as_str()) {
            let template = resource::Template::parse(op, uri)?;
            if !templates.insert(template.shape().to_vec()) {
                return Err(configuration(
                    "two operations use the same resource template shape",
                ));
            }
        }
    }
    Ok(())
}

fn register_resources<I, N, U>(
    set: &ComposedSet,
    current: &mut std::collections::BTreeMap<String, String>,
    resources: I,
) -> Result<(), Refusal>
where
    I: IntoIterator<Item = (N, U)>,
    N: Into<String>,
    U: Into<String>,
{
    let mut registered = current.clone();
    for (name, uri) in resources {
        let name = name.into();
        let uri = uri.into();
        if set.get(&name).is_none()
            || uri.is_empty()
            || uri.len() > set.bounds().message_bytes
            || registered.insert(name, uri).is_some()
        {
            return Err(configuration(
                "unknown, duplicate or oversized resource registration",
            ));
        }
    }
    validate_resources(&set.all(), &registered)?;
    *current = registered;
    Ok(())
}
