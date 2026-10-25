//! The records extension: ordered operation records, each acknowledged by the client.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use clapham::{Cancellation, Category, Evidence, OAuthContext, RecordSink, Refusal};
use rmcp::{
    ErrorData,
    model::*,
    service::{Peer, RequestContext, RoleServer},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::mapping;

/// The records extension identifier. It is also the `tools/call` `_meta` key carrying the
/// call's records token, and the tool `_meta` key marking an operation that streams records.
pub const RECORDS_EXTENSION: &str = "io.github.thirtytwobits.clapham/records";

/// The notification carrying one record, with params `{token, sequence, record}`.
pub const RECORD_NOTIFICATION: &str = "io.github.thirtytwobits.clapham/record";

/// The request acknowledging one record, with params `{token, sequence}`.
pub const ACKNOWLEDGE_REQUEST: &str = "io.github.thirtytwobits.clapham/acknowledge";

const TOKEN_BYTES: usize = 128;

/// Records awaiting acknowledgement, keyed by records token and shared by every request
/// a server handles.
#[derive(Default)]
pub(crate) struct Deliveries(Mutex<HashMap<String, Delivery>>);

struct Delivery {
    auth: OAuthContext,
    sent: u64,
    accepted: u64,
    waiting: Option<oneshot::Sender<()>>,
}

impl fmt::Debug for Deliveries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Deliveries { .. }")
    }
}

/// A call's claim on its records token, released when the call ends.
pub(crate) struct Registration {
    token: String,
    deliveries: Arc<Deliveries>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.deliveries.lock().remove(&self.token);
    }
}

impl Deliveries {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Delivery>> {
        self.0.lock().expect("delivery registry")
    }

    /// Claim `token` for a call made with `auth`, or `None` when an in-flight call holds it.
    pub(crate) fn register(
        self: &Arc<Self>,
        token: &str,
        auth: &OAuthContext,
    ) -> Option<Registration> {
        let mut deliveries = self.lock();
        if deliveries.contains_key(token) {
            return None;
        }
        deliveries.insert(
            token.to_owned(),
            Delivery {
                auth: auth.clone(),
                sent: 0,
                accepted: 0,
                waiting: None,
            },
        );
        Some(Registration {
            token: token.to_owned(),
            deliveries: self.clone(),
        })
    }

    /// The sequence the call's next record will carry.
    fn next(&self, token: &str) -> u64 {
        self.lock()[token].sent + 1
    }

    /// Record that `sequence`, the call's next record, is sent and awaits `waiting`.
    fn send(&self, token: &str, sequence: u64, waiting: oneshot::Sender<()>) {
        let mut deliveries = self.lock();
        let delivery = deliveries.get_mut(token).expect("registered token");
        debug_assert_eq!(sequence, delivery.sent + 1);
        delivery.sent = sequence;
        delivery.waiting = Some(waiting);
    }

    /// Accept the acknowledgement of the record last sent, or repeat that of the record
    /// last accepted. Only the grant that began the call may acknowledge its records.
    pub(crate) fn acknowledge(&self, auth: &OAuthContext, token: &str, sequence: u64) -> bool {
        let mut deliveries = self.lock();
        let Some(delivery) = deliveries.get_mut(token).filter(|d| d.auth == *auth) else {
            return false;
        };
        if sequence == delivery.sent && delivery.accepted < sequence {
            delivery.accepted = sequence;
            if let Some(waiting) = delivery.waiting.take() {
                let _ = waiting.send(());
            }
            true
        } else {
            sequence != 0 && sequence == delivery.accepted
        }
    }
}

/// The call's records token, when the request's client capabilities declare the extension.
pub(crate) fn token(context: &RequestContext<RoleServer>) -> Result<Option<String>, ErrorData> {
    let declared = context
        .client_capabilities()
        .and_then(|capabilities| capabilities.extensions)
        .is_some_and(|extensions| extensions.contains_key(RECORDS_EXTENSION));
    match context.meta.get(RECORDS_EXTENSION) {
        Some(_) if !declared => Ok(None),
        None => Ok(None),
        Some(Value::String(token)) if !token.is_empty() && token.len() <= TOKEN_BYTES => {
            Ok(Some(token.clone()))
        }
        Some(_) => Err(ErrorData::invalid_params("invalid records token", None)),
    }
}

/// Deliver each record as a notification on the call's response stream and wait until
/// the client acknowledges it. `limit` bounds the encoded notification.
pub(crate) fn sink(
    deliveries: Arc<Deliveries>,
    token: String,
    peer: Peer<RoleServer>,
    cancel: Cancellation,
    limit: usize,
) -> RecordSink {
    let serial = Arc::new(tokio::sync::Mutex::new(()));
    RecordSink::new(move |record| {
        let (deliveries, token, peer, cancel, serial) = (
            deliveries.clone(),
            token.clone(),
            peer.clone(),
            cancel.clone(),
            serial.clone(),
        );
        async move {
            let _serial = serial.lock().await;
            // A refused record is never sent, so it takes no sequence.
            let sequence = deliveries.next(&token);
            let notification = ServerNotification::CustomNotification(CustomNotification::new(
                RECORD_NOTIFICATION,
                Some(json!({"token":token,"sequence":sequence,"record":record})),
            ));
            if mapping::size(
                &ServerJsonRpcMessage::notification(notification.clone()),
                limit,
            )
            .is_none()
            {
                return Err(record_too_large());
            }
            let (accepted, received) = oneshot::channel();
            deliveries.send(&token, sequence, accepted);
            peer.send_notification(notification)
                .await
                .map_err(|_| delivery_failed())?;
            tokio::select! {
                biased;
                () = cancel.cancelled() => Err(delivery_failed()),
                accepted = received => accepted.map_err(|_| delivery_failed()),
            }
        }
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgement {
    token: String,
    sequence: u64,
}

/// Answer an acknowledgement request from the grant `auth`.
pub(crate) fn acknowledge(
    deliveries: &Deliveries,
    auth: &OAuthContext,
    request: CustomRequest,
) -> Result<CustomResult, ErrorData> {
    if request.method != ACKNOWLEDGE_REQUEST {
        return Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            request.method,
            None,
        ));
    }
    let Ok(Some(Acknowledgement { token, sequence })) = request.params_as::<Acknowledgement>()
    else {
        return Err(ErrorData::invalid_params(
            "invalid record acknowledgement",
            None,
        ));
    };
    if deliveries.acknowledge(auth, &token, sequence) {
        Ok(CustomResult::new(json!({})))
    } else {
        Err(ErrorData::invalid_params(
            "the record acknowledgement is unknown",
            None,
        ))
    }
}

/// Whether `message` is a request acknowledging a record.
pub(crate) fn is_acknowledgement(message: &ClientJsonRpcMessage) -> bool {
    matches!(message, JsonRpcMessage::Request(request)
        if matches!(&request.request, ClientRequest::CustomRequest(custom) if custom.method == ACKNOWLEDGE_REQUEST))
}

fn record_too_large() -> Refusal {
    Refusal::new(
        "result_too_large",
        Category::Usage,
        "a record notification exceeds bounds.message_bytes",
    )
    .with_evidence(Evidence::Uncertain)
}

fn delivery_failed() -> Refusal {
    Refusal::new(
        "record_delivery_failed",
        Category::Unavailable,
        "the record was not acknowledged",
    )
    .with_evidence(Evidence::Uncertain)
}
