//! Copyright (c) Scott A Dixon
//! The records extension: route each record to its call and acknowledge it.
use crate::McpError;
use rmcp::{
    RoleClient,
    model::*,
    service::{Peer, ServiceError},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

pub(crate) const RECORDS_EXTENSION: &str = "io.github.thirtytwobits.clapham/records";
pub(crate) const RECORD_NOTIFICATION: &str = "io.github.thirtytwobits.clapham/record";
const ACKNOWLEDGE_REQUEST: &str = "io.github.thirtytwobits.clapham/acknowledge";

type Records = mpsc::UnboundedSender<(u64, Value)>;

/// Calls awaiting records, keyed by records token. rmcp runs notification handlers
/// unordered; the server sends a call's next record only after the last is acknowledged.
#[derive(Default)]
pub(crate) struct Routes(Mutex<HashMap<String, Records>>);

/// A call's records, routed until the call ends.
pub(crate) struct Route {
    pub token: String,
    pub records: mpsc::UnboundedReceiver<(u64, Value)>,
    routes: Arc<Routes>,
}
impl Drop for Route {
    fn drop(&mut self) {
        self.routes.0.lock().unwrap().remove(&self.token);
    }
}

impl Routes {
    pub fn open(self: &Arc<Self>) -> Route {
        let token = uuid::Uuid::new_v4().to_string();
        let (sender, records) = mpsc::unbounded_channel();
        self.0.lock().unwrap().insert(token.clone(), sender);
        Route {
            token,
            records,
            routes: self.clone(),
        }
    }
    /// Pass a record notification to its call. Records for no in-flight call are dropped.
    pub fn deliver(&self, notification: CustomNotification) {
        let Some(params) = notification.params else {
            return;
        };
        let (Some(token), Some(sequence), Some(record)) = (
            params.get("token").and_then(Value::as_str),
            params.get("sequence").and_then(Value::as_u64),
            params.get("record"),
        ) else {
            return;
        };
        if let Some(records) = self.0.lock().unwrap().get(token) {
            let _ = records.send((sequence, record.clone()));
        }
    }
}

/// The client capabilities each request declares.
pub(crate) fn capabilities() -> ClientCapabilities {
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some(ExtensionCapabilities::from([(
        RECORDS_EXTENSION.to_owned(),
        JsonObject::new(),
    )]));
    capabilities
}

/// Acknowledge one record. A transport failure leaves the first attempt's fate unknown,
/// so the acknowledgement, which the server accepts repeatedly, is sent once more.
pub(crate) async fn acknowledge(
    peer: &Peer<RoleClient>,
    token: &str,
    sequence: u64,
) -> Result<(), McpError> {
    let request = || {
        ClientRequest::CustomRequest(CustomRequest::new(
            ACKNOWLEDGE_REQUEST,
            Some(json!({"token":token,"sequence":sequence})),
        ))
    };
    match peer.send_request(request()).await {
        Ok(_) => Ok(()),
        Err(ServiceError::McpError(error)) => Err(McpError::Invocation(error.code.0)),
        Err(_) => peer
            .send_request(request())
            .await
            .map(drop)
            .map_err(McpError::from_service),
    }
}
