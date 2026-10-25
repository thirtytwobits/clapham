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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::mpsc;

pub(crate) const RECORDS_EXTENSION: &str = "io.github.thirtytwobits.clapham/records";
pub(crate) const RECORD_NOTIFICATION: &str = "io.github.thirtytwobits.clapham/record";
const ACKNOWLEDGE_REQUEST: &str = "io.github.thirtytwobits.clapham/acknowledge";

struct Entry {
    records: mpsc::UnboundedSender<(u64, Value)>,
    pending: Arc<AtomicBool>,
}

/// Calls awaiting records, keyed by records token. rmcp runs notification handlers
/// unordered; the server sends a call's next record only after the last is acknowledged.
///
/// Acknowledgement paces a call's records, so the record a call awaits is outside the
/// connection's lifetime notification and byte budgets. A record that arrives while the
/// call holds another is counted.
#[derive(Default)]
pub(crate) struct Routes(Mutex<HashMap<String, Entry>>);

/// A call's records, routed until the call ends.
pub(crate) struct Route {
    pub token: String,
    pub records: mpsc::UnboundedReceiver<(u64, Value)>,
    pending: Arc<AtomicBool>,
    routes: Arc<Routes>,
}
impl Route {
    /// Release the record in hand before acknowledging it, so the next is admitted.
    pub fn release(&self) {
        self.pending.store(false, Ordering::SeqCst);
    }
}
impl Drop for Route {
    fn drop(&mut self) {
        self.routes.0.lock().unwrap().remove(&self.token);
    }
}

/// The records token of a record notification.
fn record_token(message: &ServerJsonRpcMessage) -> Option<&str> {
    let ServerJsonRpcMessage::Notification(notification) = message else {
        return None;
    };
    let ServerNotification::CustomNotification(custom) = &notification.notification else {
        return None;
    };
    if custom.method != RECORD_NOTIFICATION {
        return None;
    }
    custom.params.as_ref()?.get("token")?.as_str()
}

impl Routes {
    pub fn open(self: &Arc<Self>) -> Route {
        let token = uuid::Uuid::new_v4().to_string();
        let (sender, records) = mpsc::unbounded_channel();
        let pending = Arc::new(AtomicBool::new(false));
        self.0.lock().unwrap().insert(
            token.clone(),
            Entry {
                records: sender,
                pending: pending.clone(),
            },
        );
        Route {
            token,
            records,
            pending,
            routes: self.clone(),
        }
    }
    /// Whether `token` names a call that holds no record.
    pub fn awaits(&self, token: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(token)
            .is_some_and(|entry| !entry.pending.load(Ordering::SeqCst))
    }
    /// Whether `message` is the record its call awaits, which the lifetime byte budget
    /// does not charge.
    pub fn paced(&self, message: &ServerJsonRpcMessage) -> bool {
        record_token(message).is_some_and(|token| self.awaits(token))
    }
    /// As [`Self::paced`], and mark the record as the one its call holds, which the
    /// lifetime notification budget does not count.
    pub fn admit(&self, message: &ServerJsonRpcMessage) -> bool {
        let Some(token) = record_token(message) else {
            return false;
        };
        self.0.lock().unwrap().get(token).is_some_and(|entry| {
            entry
                .pending
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        })
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
        if let Some(entry) = self.0.lock().unwrap().get(token) {
            let _ = entry.records.send((sequence, record.clone()));
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
