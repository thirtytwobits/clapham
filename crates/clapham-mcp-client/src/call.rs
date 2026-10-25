//! Copyright (c) Scott A Dixon
//! Tool calls: records, acknowledgement, deadlines and dispatch evidence.
use crate::{McpConnection, McpError, records, wire};
use clapham::{Evidence, Outcome, RecordSink, Refusal};
use rmcp::{model::*, service::ServiceError};
use serde_json::json;
use tokio::time::{Instant, timeout_at};

/// Tool-result metadata from a Clapham server naming the outcome's absent fields.
const ABSENT_META_KEY: &str = "io.github.thirtytwobits.clapham/absent";
/// Tool-result metadata from a Clapham server marking a refusal.
const REFUSAL_META_KEY: &str = "io.github.thirtytwobits.clapham/refusal";

/// Run `future`, or stop at `deadline` when there is one.
async fn within<F: Future>(deadline: Option<Instant>, future: F) -> Option<F::Output> {
    match deadline {
        Some(deadline) => timeout_at(deadline, future).await.ok(),
        None => Some(future.await),
    }
}

impl McpConnection {
    /// Call a remote tool by its remote name, outside any binding, until its result
    /// arrives. `meta` joins the request's `_meta`. Records reach `records` under the
    /// records extension, each acknowledged after the sink accepts it.
    ///
    /// No deadline applies: the caller bounds the call, and dropping the future cancels it.
    /// A tool-reported error is an outcome with `is_error`. A Clapham server's refusal, as a
    /// marked tool result or a structured HTTP refusal such as for missing OAuth scopes or a
    /// full queue, is returned unchanged.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: JsonObject,
        meta: JsonObject,
        records: RecordSink,
    ) -> Result<Outcome, Refusal> {
        self.check()
            .map_err(|error| error.refusal(Evidence::NotDispatched))?;
        self.invoke(name, arguments, meta, &records, None, true, || self.check())
            .await
    }

    /// Call `name` once admitted. `check` runs again after queueing, before dispatch. With
    /// `server_refusals`, a server's structured refusal before dispatch is returned unchanged;
    /// otherwise its bounded diagnostic is.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn invoke(
        &self,
        name: &str,
        arguments: JsonObject,
        meta: JsonObject,
        records: &RecordSink,
        deadline: Option<Instant>,
        server_refusals: bool,
        check: impl Fn() -> Result<(), McpError>,
    ) -> Result<Outcome, Refusal> {
        let refused = |error: McpError| error.refusal(Evidence::NotDispatched);
        let mut route = self.0.state.routes.open();
        let mut request_meta = RequestMetaObject::new();
        request_meta.extend(RequestMetaObject::from(MetaObject::from(meta)));
        request_meta.insert(records::RECORDS_EXTENSION.into(), json!(route.token));
        let mut params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        params.meta = Some(request_meta);
        // Reserve room for the SDK's per-request metadata and JSON-RPC envelope.
        wire::encode(
            &params,
            self.0
                .config
                .bounds
                .message_bytes
                .saturating_sub(crate::connection::REQUEST_ENVELOPE_RESERVE),
        )
        .map_err(refused)?;
        let _ticket = self
            .0
            .admitted
            .try_acquire()
            .map_err(|_| refused(McpError::Busy))?;
        let _active = within(deadline, self.0.active.acquire())
            .await
            .ok_or_else(|| refused(McpError::Timeout))?
            .map_err(|_| refused(McpError::Transport))?;
        check().map_err(refused)?;
        let peer = self.0.service.peer().clone();
        // Dropping the reply future cancels the call.
        let reply = peer.call_tool_once(params);
        tokio::pin!(reply);
        let streaming = async {
            let mut expected = 1;
            loop {
                tokio::select! {
                    biased;
                    response = &mut reply => return Ok(response),
                    Some((sequence, record)) = route.records.recv() => {
                        if sequence != expected {
                            return Err(McpError::Protocol.refusal(Evidence::Uncertain));
                        }
                        expected += 1;
                        records.send(record).await?;
                        // The server sends the next record only after this acknowledgement.
                        route.release();
                        // The result is authoritative: it can arrive while the reply to
                        // the acknowledgement that released it is still outstanding.
                        let acknowledged = records::acknowledge(&peer, &route.token, sequence);
                        tokio::pin!(acknowledged);
                        tokio::select! {
                            biased;
                            response = &mut reply => return Ok(response),
                            acknowledged = &mut acknowledged => if let Err(error) = acknowledged {
                                // An acknowledgement that cannot be confirmed cancels the
                                // call, unless its result has already arrived.
                                if let Some(response) = futures_util::FutureExt::now_or_never(&mut reply) {
                                    return Ok(response);
                                }
                                let error = self.0.state.check().err().unwrap_or(error);
                                return Err(error.refusal(Evidence::Uncertain));
                            },
                        }
                    }
                }
            }
        };
        let response = match within(deadline, streaming).await {
            None => return Err(McpError::Timeout.refusal(Evidence::Uncertain)),
            Some(Err(refusal)) => return Err(refusal),
            Some(Ok(response)) => response,
        };
        let response = match response {
            Err(error) => {
                if server_refusals && let Some(refusal) = McpError::server_refusal(&error) {
                    return Err(refusal);
                }
                let completed = matches!(
                    error,
                    ServiceError::McpError(_) | ServiceError::UnexpectedResponse
                );
                let error = self
                    .0
                    .state
                    .check()
                    .err()
                    .unwrap_or_else(|| McpError::from_service(error));
                // The server authorises and admits a request before reading it.
                let outcome = match error {
                    McpError::Unauthenticated | McpError::Forbidden | McpError::Busy => {
                        Evidence::NotDispatched
                    }
                    _ if completed => Evidence::Completed,
                    _ => Evidence::Uncertain,
                };
                return Err(error.refusal(outcome));
            }
            Ok(response) => response,
        };
        match response {
            CallToolResponse::Complete(result) => {
                if server_refusals && let Some(refusal) = marked_refusal(&result) {
                    return Err(refusal);
                }
                convert(result).map_err(|e| e.refusal(Evidence::Completed))
            }
            _ => Err(McpError::UnsupportedInteraction.refusal(Evidence::Uncertain)),
        }
    }
}

/// The refusal a Clapham server marked as such, as distinct from an error outcome.
fn marked_refusal(result: &CallToolResult) -> Option<Refusal> {
    let marked = result
        .meta
        .as_ref()
        .and_then(|meta| meta.get(REFUSAL_META_KEY))
        == Some(&serde_json::Value::Bool(true));
    if !marked || result.is_error != Some(true) {
        return None;
    }
    serde_json::from_value(result.structured_content.clone()?).ok()
}

fn convert(result: CallToolResult) -> Result<Outcome, McpError> {
    let evidence = result
        .meta
        .as_ref()
        .and_then(|meta| meta.get("io.github.thirtytwobits.clapham/evidence"))
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| McpError::Protocol)?
        .unwrap_or(Evidence::Completed);
    // A Clapham server names the outcome fields that MCP cannot show to be absent.
    let absent = result
        .meta
        .as_ref()
        .and_then(|meta| meta.get(ABSENT_META_KEY))
        .cloned()
        .map(serde_json::from_value::<Vec<String>>)
        .transpose()
        .map_err(|_| McpError::Protocol)?;
    let is_absent = |field: &str| {
        absent
            .as_ref()
            .is_some_and(|absent| absent.iter().any(|name| name == field))
    };
    let blocks = result
        .content
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| McpError::Protocol)?;
    let content = if is_absent("content") && blocks.is_empty() {
        None
    } else {
        Some(blocks)
    };
    let structured = match (&absent, result.structured_content) {
        (Some(_), None) if !is_absent("result") => Some(serde_json::Value::Null),
        (_, structured) => structured,
    };
    Ok(Outcome {
        content,
        result: structured,
        is_error: result.is_error.unwrap_or(false),
        evidence,
    })
}
