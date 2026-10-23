#![doc = include_str!("../README.md")]

mod mapping;
mod resource;
#[cfg(feature = "stdio")]
mod stdio;

use std::{borrow::Cow, sync::Arc};

use clapham::{
    Cancellation, Category, ComposedSet, Operation, Principal, RecordSink, Refusal, Selection,
};
use rmcp::{
    ErrorData, ServerHandler,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Request metadata key selecting a dry run.
pub const PREVIEW_META_KEY: &str = "io.github.thirtytwobits.clapham/preview";

/// MCP 2026-07-28 over a selection, bound to one application-supplied principal.
///
/// Clones share admission and execution state. Identity never comes from MCP input.
#[derive(Debug, Clone)]
pub struct Server {
    set: ComposedSet,
    selection: Selection,
    discovery: Selection,
    principal: Principal,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Server {
    /// Expose all operations. Resource templates must use simple string variables
    /// (`lists://{id}`); unsupported or ambiguous templates refuse configuration.
    pub fn new(set: ComposedSet, principal: Principal) -> Result<Self, Refusal> {
        let selection = set.all();
        let slots = Arc::new(tokio::sync::Semaphore::new(set.bounds().in_flight));
        let server = Self {
            discovery: selection.clone(),
            selection,
            set,
            principal,
            slots,
        };
        server.validate_resources()?;
        Ok(server)
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
        let mut templates = std::collections::BTreeSet::new();
        for op in self.selection.operations() {
            if let Some(uri) = &op.resource {
                resource::Template::parse(op)?;
                if !templates.insert(uri) {
                    return Err(configuration(
                        "two operations use the same resource template",
                    ));
                }
            }
        }
        Ok(())
    }

    // Named describes avoid forcing the complete catalogue into one core result.
    // The same gate filters discovery and dispatch, including custom admission policy.
    async fn catalogue(&self) -> Result<Vec<Operation>, ErrorData> {
        let mut operations = Vec::new();
        for op in self.selection.operations() {
            let result = self
                .set
                .call(
                    &self.discovery,
                    self.principal.clone(),
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

    async fn invoke(
        &self,
        name: &str,
        input: Value,
        preview: bool,
        context: &RequestContext<RoleServer>,
    ) -> clapham::CallResult {
        let cancel = Cancellation::new();
        if context.ct.is_cancelled() {
            cancel.cancel();
        }
        let call = self.set.call(
            &self.selection,
            self.principal.clone(),
            name,
            input,
            RecordSink::default(),
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

    fn page<T: Serialize + Clone>(
        &self,
        kind: &str,
        items: &[T],
        request: Option<PaginatedRequestParams>,
        context: &RequestContext<RoleServer>,
    ) -> Result<(Vec<T>, Option<String>), ErrorData> {
        let bounds = self.set.bounds();
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
        // Reserve the envelope, lifecycle metadata and cursor; account separately
        // for an arbitrary request ID. The transport checks the final frame too.
        let id_size =
            mapping::size(&context.id, bounds.message_bytes).unwrap_or(bounds.message_bytes);
        let budget = bounds.message_bytes.saturating_sub(1024 + id_size);
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
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
        .with_server_info(Implementation::new("clapham", env!("CARGO_PKG_VERSION")))
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    async fn discover(&self, _: RequestContext<RoleServer>) -> Result<DiscoverResult, ErrorData> {
        let operations = self.catalogue().await?;
        let mut capabilities = ServerCapabilities::default();
        if !operations.is_empty() {
            capabilities.tools = self.get_info().capabilities.tools;
        }
        if operations.iter().any(|op| op.resource.is_some()) {
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
            .catalogue()
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
        let permit = tokio::select! {
            biased;
            () = context.ct.cancelled() => None,
            permit = self.slots.acquire() => Some(permit.map_err(|_| ErrorData::internal_error("server closed", None))?),
        };
        let outcome = self
            .invoke(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
                preview,
                &context,
            )
            .await;
        drop(permit);
        let result = match outcome {
            Ok(outcome) => mapping::outcome(outcome),
            Err(refusal) => mapping::refusal(refusal),
        };
        Ok(result.into())
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let templates = self
            .catalogue()
            .await?
            .iter()
            .filter_map(|op| {
                op.resource.as_ref().map(|uri| {
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
            .catalogue()
            .await?
            .iter()
            .filter_map(|op| {
                op.resource
                    .as_ref()
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
        let operations = self.catalogue().await?;
        let exact = operations
            .iter()
            .any(|op| op.resource.as_deref() == Some(request.uri.as_str()));
        let mut matched = None;
        for op in operations {
            if exact && op.resource.as_deref() != Some(request.uri.as_str()) {
                continue;
            }
            if op.resource.is_some() {
                let template = resource::Template::parse(&op).map_err(protocol_refusal)?;
                if let Some(input) = template.bind(&request.uri) {
                    if matched.is_some() {
                        return Err(ErrorData::invalid_params("ambiguous resource URI", None));
                    }
                    matched = Some((op.name.to_string(), input));
                }
            }
        }
        let (name, input) =
            matched.ok_or_else(|| ErrorData::resource_not_found("resource not found", None))?;
        let _permit = tokio::select! {
            biased;
            () = context.ct.cancelled() => None,
            permit = self.slots.acquire() => Some(permit.map_err(|_| ErrorData::internal_error("server closed", None))?),
        };
        let outcome = self
            .invoke(&name, Value::Object(input), false, &context)
            .await;
        // resources/read has no tool-error union. Carry the same Refusal as JSON
        // resource contents so operation failures are not recast as protocol faults.
        let value = match outcome {
            Ok(outcome) => outcome.result.unwrap_or(Value::Null),
            Err(refusal) => serde_json::to_value(refusal).expect("refusals serialize"),
        };
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(value.to_string(), request.uri)
                .with_mime_type("application/json"),
        ])
        .with_ttl_ms(0)
        .with_cache_scope(CacheScope::Private)
        .into())
    }
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
