//! Copyright (c) Scott A Dixon
//! Bounded wire admission shared by both transports.
use crate::McpError;
use serde::Serialize;
use std::io::Write;

pub(crate) fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, McpError> {
    struct Buffer {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("MCP message limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut buffer, value).map_err(|_| McpError::Limit)?;
    Ok(buffer.bytes)
}

#[cfg(any(feature = "stdio", feature = "http"))]
pub(crate) use responses::Requests;

#[cfg(any(feature = "stdio", feature = "http"))]
mod responses {
    use crate::McpError;
    use rmcp::model::*;
    use serde_json::Value;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Copy)]
    enum Kind {
        Catalogue,
        Operations,
        Call,
    }

    // The adapter admits at most 32 active operations. Keep correlation bounded even if a
    // failed peer never answers or acknowledges cancellation. No request payload is retained.
    #[derive(Clone, Default)]
    pub(crate) struct Requests(Arc<Mutex<HashMap<RequestId, Kind>>>);

    pub(crate) struct Sending {
        requests: Requests,
        id: Option<RequestId>,
    }
    impl Drop for Sending {
        fn drop(&mut self) {
            if let Some(id) = &self.id {
                self.requests.0.lock().unwrap().remove(id);
            }
        }
    }
    impl Sending {
        pub fn sent(mut self) {
            self.id = None;
        }
    }
    impl Requests {
        pub fn begin(&self, message: &ClientJsonRpcMessage) -> Result<Sending, McpError> {
            if let ClientJsonRpcMessage::Notification(notification) = message
                && let ClientNotification::CancelledNotification(cancel) =
                    &notification.notification
                && let Some(id) = &cancel.params.request_id
            {
                self.0.lock().unwrap().remove(id);
            }
            let id = if let ClientJsonRpcMessage::Request(request) = message {
                let kind = match &request.request {
                    ClientRequest::ListToolsRequest(_) => Some(Kind::Catalogue),
                    ClientRequest::CallToolRequest(_) => Some(Kind::Call),
                    ClientRequest::CustomRequest(custom)
                        if custom.method == crate::operations::CATALOGUE_EXTENSION =>
                    {
                        Some(Kind::Operations)
                    }
                    _ => None,
                };
                if let Some(kind) = kind {
                    let mut requests = self.0.lock().unwrap();
                    if requests.len() >= 32 || requests.contains_key(&request.id) {
                        return Err(McpError::Limit);
                    }
                    requests.insert(request.id.clone(), kind);
                    Some(request.id.clone())
                } else {
                    None
                }
            } else {
                None
            };
            Ok(Sending {
                requests: self.clone(),
                id,
            })
        }

        pub fn prepare(&self, message: &mut Value) {
            if message.get("method").is_some()
                || (message.get("result").is_none() && message.get("error").is_none())
            {
                return;
            }
            let Some(id) = message
                .get("id")
                .cloned()
                .and_then(|id| serde_json::from_value::<RequestId>(id).ok())
            else {
                return;
            };
            let kind = self.0.lock().unwrap().remove(&id);
            let Some(result) = message.get_mut("result") else {
                return;
            };
            match kind {
                Some(Kind::Catalogue) => {
                    // One representation across stdio, HTTP JSON and SSE. This prevents the
                    // SDK's untagged union from discarding malformed records or unknown fields.
                    *result = serde_json::json!({"claphamCatalogue": result.take()});
                }
                Some(Kind::Operations) => {
                    // As above, for a Clapham server's operation catalogue.
                    *result = serde_json::json!({"claphamOperations": result.take()});
                }
                Some(Kind::Call) => {
                    // Interpret a call by its request, before the SDK's untagged result union
                    // can mistake extension fields (e.g. `tools`) for another result family.
                    let call = match result.get("resultType").and_then(Value::as_str) {
                        Some("input_required") => {
                            serde_json::from_value::<InputRequiredResult>(result.clone())
                                .and_then(serde_json::to_value)
                        }
                        Some("task") => serde_json::from_value::<CreateTaskResult>(result.clone())
                            .and_then(serde_json::to_value),
                        _ => serde_json::from_value::<CallToolResult>(result.clone())
                            .and_then(serde_json::to_value),
                    };
                    if let Ok(call) = call {
                        *result = call;
                    }
                }
                None => {}
            }
        }
    }
}

#[cfg(any(feature = "stdio", feature = "http"))]
pub(crate) fn decode(
    bytes: &[u8],
    requests: &Requests,
) -> Result<rmcp::model::ServerJsonRpcMessage, McpError> {
    let mut raw = serde_json::from_slice(bytes).map_err(|_| McpError::Protocol)?;
    requests.prepare(&mut raw);
    serde_json::from_value(raw).map_err(|_| McpError::Protocol)
}
