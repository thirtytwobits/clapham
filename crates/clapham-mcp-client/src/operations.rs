//! Copyright (c) Scott A Dixon
//! A Clapham server's operation catalogue, under its catalogue extension.
use crate::{McpConnection, McpError, wire};
use clapham::Operation;
use rmcp::model::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::time::{Instant, timeout_at};

pub(crate) const CATALOGUE_EXTENSION: &str = "io.github.thirtytwobits.clapham/catalogue";

impl McpConnection {
    /// The operation descriptors a Clapham server admits to this connection's grant from its
    /// selection, whether or not the selection exposes `clapham.describe`.
    ///
    /// Pages, catalogue bytes and the operation count are bounded by the connection's
    /// `Bounds`, and `operation_timeout` covers the whole catalogue. An abandoned or expired
    /// read closes the connection, so its request state cannot outlive it.
    pub async fn operations(&self) -> Result<Vec<Operation>, McpError> {
        self.check()?;
        let deadline = Instant::now() + self.0.config.operation_timeout;
        let _ticket = self.0.admitted.try_acquire().map_err(|_| McpError::Busy)?;
        let mut cancel_on_drop = None;
        let work = async {
            let _active = self
                .0
                .active
                .acquire()
                .await
                .map_err(|_| McpError::Transport)?;
            // Raw catalogue requests have no cancellation of their own.
            cancel_on_drop = Some(self.0.state.stop.clone().drop_guard());
            let limits = &self.0.config.bounds;
            let mut operations = Vec::new();
            let mut cursor: Option<String> = None;
            let mut cursors = BTreeSet::new();
            let mut bytes = 0usize;
            for page in 0..limits.pages {
                let params = match &cursor {
                    Some(cursor) => json!({"cursor":cursor}),
                    None => json!({}),
                };
                let request = ClientRequest::CustomRequest(CustomRequest::new(
                    CATALOGUE_EXTENSION,
                    Some(params),
                ));
                let response = self
                    .0
                    .service
                    .peer()
                    .send_request(request)
                    .await
                    .map_err(McpError::from_service)?;
                let value = serde_json::to_value(response).map_err(|_| McpError::Protocol)?;
                let value = value.get("claphamOperations").ok_or(McpError::Protocol)?;
                bytes = bytes
                    .checked_add(wire::encode(value, limits.catalogue_bytes)?.len())
                    .filter(|bytes| *bytes <= limits.catalogue_bytes)
                    .ok_or(McpError::Limit)?;
                let page_operations: Vec<Operation> =
                    serde_json::from_value(value.get("operations").cloned().unwrap_or_default())
                        .map_err(|_| McpError::Protocol)?;
                operations.extend(page_operations);
                if operations.len() > limits.tools {
                    return Err(McpError::Limit);
                }
                cursor = match value.get("nextCursor") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(cursor)) => Some(cursor.clone()),
                    _ => return Err(McpError::Protocol),
                };
                match &cursor {
                    Some(cursor) if !cursors.insert(cursor.clone()) || page + 1 == limits.pages => {
                        return Err(McpError::Limit);
                    }
                    Some(_) => {}
                    None => return Ok(operations),
                }
            }
            Err(McpError::Limit)
        };
        let result = timeout_at(deadline, work).await;
        if let Some(guard) = cancel_on_drop {
            guard.disarm();
        }
        result.unwrap_or_else(|_| {
            self.0.state.fail(McpError::Timeout);
            Err(McpError::Timeout)
        })
    }
}
