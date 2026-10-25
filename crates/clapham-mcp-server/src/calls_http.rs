//! Authenticated operation HTTP adapter with ordered NDJSON records.

use super::*;
use clapham::{Cancellation, RecordSink};
use http_body::Frame;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    name: String,
    input: Value,
    #[serde(default)]
    preview: bool,
}

#[derive(Default)]
pub(super) struct Acknowledgements {
    pending: std::collections::BTreeMap<String, PendingRecord>,
}
struct PendingRecord {
    auth: OAuthContext,
    received: oneshot::Sender<()>,
}
struct PendingAcknowledgement {
    ticket: String,
    registry: Arc<std::sync::Mutex<Acknowledgements>>,
}
impl Drop for PendingAcknowledgement {
    fn drop(&mut self) {
        self.registry
            .lock()
            .expect("acknowledgement registry")
            .pending
            .remove(&self.ticket);
    }
}

struct Records {
    receiver: mpsc::Receiver<(Bytes, oneshot::Sender<()>)>,
    acknowledgement: Option<oneshot::Sender<()>>,
    cancel: Cancellation,
}

impl Body for Records {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        if let Some(acknowledgement) = self.acknowledgement.take() {
            let _ = acknowledgement.send(());
        }
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some((bytes, acknowledgement))) => {
                self.acknowledgement = Some(acknowledgement);
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for Records {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(super) fn full(response: Response<Full<Bytes>>) -> Response<BoxBody<Bytes, Infallible>> {
    response.map(BodyExt::boxed)
}

async fn send(
    sender: &mpsc::Sender<(Bytes, oneshot::Sender<()>)>,
    kind: &str,
    data: Value,
    limit: usize,
    acknowledgement: Option<&str>,
) -> Result<(), Refusal> {
    let mut frame = json!({"kind":kind,"data":data});
    if let Some(ticket) = acknowledgement {
        frame["ack"] = json!(ticket);
    }
    let mut bytes = serde_json::to_vec(&frame).expect("frames serialise");
    bytes.push(b'\n');
    if bytes.len() > limit {
        return Err(Refusal::new(
            "result_too_large",
            Category::Usage,
            "operation frame exceeds bounds.message_bytes",
        )
        .with_evidence(Evidence::Uncertain));
    }
    let (acknowledgement, received) = oneshot::channel();
    sender
        .send((Bytes::from(bytes), acknowledgement))
        .await
        .map_err(|_| delivery_error())?;
    received.await.map_err(|_| delivery_error())
}

fn delivery_error() -> Refusal {
    Refusal::new(
        "record_delivery_failed",
        Category::Unavailable,
        "the operation response was disconnected",
    )
    .with_evidence(Evidence::Uncertain)
}

impl HttpServer {
    /// Mount MCP and the operation/record interface under the same authenticator.
    /// The application supplies the listener, TLS and shutdown to its HTTP host.
    pub fn into_router(self) -> axum::Router {
        let mcp = self.clone();
        axum::Router::new()
            .route(
                "/mcp",
                axum::routing::any(move |request| {
                    let server = mcp.clone();
                    async move { server.handle(request).await }
                }),
            )
            .fallback(axum::routing::any(move |request| {
                let server = self.clone();
                async move { server.handle_operations(request).await }
            }))
    }

    fn acknowledge<B>(&self, request: Request<B>, auth: OAuthContext) -> Response<Full<Bytes>>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: fmt::Display,
    {
        let ticket = request
            .uri()
            .path()
            .strip_prefix("/ack/")
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
            .map(|id| id.to_string());
        let Some(ticket) = ticket else {
            return refusal_response(
                StatusCode::NOT_FOUND,
                "unknown_record",
                Category::NotFound,
                "the record acknowledgement is unknown",
            );
        };
        if !request.body().is_end_stream() {
            return refusal_response(
                StatusCode::BAD_REQUEST,
                "invalid_input",
                Category::InvalidInput,
                "acknowledgements require an empty, completed body",
            );
        }
        let pending = {
            let mut registry = self
                .acknowledgements
                .lock()
                .expect("acknowledgement registry");
            if registry
                .pending
                .get(&ticket)
                .is_some_and(|record| record.auth == auth)
            {
                registry.pending.remove(&ticket)
            } else {
                None
            }
        };
        match pending.is_some_and(|pending| pending.received.send(()).is_ok()) {
            true => Response::builder()
                .status(StatusCode::NO_CONTENT)
                .body(Full::new(Bytes::new()))
                .expect("valid acknowledgement"),
            _ => refusal_response(
                StatusCode::NOT_FOUND,
                "unknown_record",
                Category::NotFound,
                "the record acknowledgement is unknown",
            ),
        }
    }

    /// Serve the transport-independent operation interface.
    ///
    /// `GET /operations` returns admitted descriptors. `POST /call` accepts
    /// `{name,input,preview}` and emits ordered record frames followed by one
    /// outcome or refusal. Each record has a one-use ticket; the producer waits
    /// for the caller's authenticated acknowledgement after sink acceptance.
    /// Disconnect drops the response and cancels the shared call runner.
    /// Mount alongside the MCP endpoint under the same OAuth authenticator.
    pub async fn handle_operations<B>(
        &self,
        request: Request<B>,
    ) -> Response<BoxBody<Bytes, Infallible>>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: fmt::Display,
    {
        let limit = self.set.bounds().message_bytes;
        let validator = StreamableHttpService::<Server, _>::new(
            || Err(std::io::Error::other("HTTP policy probe cannot dispatch")),
            Arc::new(LocalSessionManager::default()),
            self.transport_config(),
        );
        let mut probe = Request::new(Empty::<Bytes>::new());
        *probe.method_mut() = http::Method::OPTIONS;
        *probe.uri_mut() = request.uri().clone();
        *probe.headers_mut() = request.headers().clone();
        let checked = validator.handle(probe).await;
        if checked.status() != StatusCode::METHOD_NOT_ALLOWED {
            return full(bounded_response(checked, limit, false).await);
        }
        if request.method() == http::Method::GET
            && self.metadata_path.as_deref() == Some(request.uri().path())
            && let Some(resource) = self.set.oauth_resource()
        {
            let metadata =
                json!({"resource":resource.audience,"authorization_servers":[resource.issuer]});
            return full(
                Response::builder()
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from(metadata.to_string())))
                    .expect("valid metadata response"),
            );
        }
        let mut authorization = request.headers().get_all(header::AUTHORIZATION).iter();
        let credential = match (authorization.next(), authorization.next()) {
            (None, None) => Credential::default(),
            (Some(value), None) => match value.to_str() {
                Ok(value) => Credential::new(value),
                Err(_) => {
                    return full(refusal_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        Category::InvalidInput,
                        "malformed Authorization field",
                    ));
                }
            },
            _ => {
                return full(refusal_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_credential",
                    Category::InvalidInput,
                    "multiple Authorization fields are not permitted",
                ));
            }
        };
        let Ok(authentication) = self.authenticating.try_acquire() else {
            return full(refusal_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_busy",
                Category::Unavailable,
                "authentication queue is full",
            ));
        };
        let Some(auth) = self
            .authenticator
            .authenticate(&credential)
            .await
            .filter(|auth| {
                self.set
                    .oauth_resource()
                    .is_some_and(|resource| auth.is_for(resource))
            })
        else {
            let mut response = refusal_response(
                StatusCode::UNAUTHORIZED,
                "not_authenticated",
                Category::Refused,
                "the OAuth token was not verified",
            );
            if let Some(challenge) = &self.authentication_challenge {
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, challenge.clone());
            }
            return full(response);
        };
        drop(authentication);
        if request.method() == http::Method::POST && request.uri().path().starts_with("/ack/") {
            return full(self.acknowledge(request, auth));
        }
        let Ok(outstanding) = self.outstanding.clone().try_acquire_owned() else {
            return full(refusal_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_busy",
                Category::Unavailable,
                "operation queue is full",
            ));
        };
        if request.method() == http::Method::GET && request.uri().path() == "/operations" {
            let result = self
                .set
                .call(
                    &self.discovery,
                    auth,
                    "clapham.describe",
                    json!({}),
                    RecordSink::default(),
                    Cancellation::new(),
                    false,
                )
                .await;
            return full(match result {
                Ok(mut outcome) => {
                    if let Some(serde_json::Value::Array(operations)) = &mut outcome.result {
                        operations.retain(|operation| {
                            operation["name"]
                                .as_str()
                                .is_some_and(|name| self.selection.contains(name))
                        });
                    }
                    Response::builder()
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Full::new(Bytes::from(
                            outcome.result.expect("describe result").to_string(),
                        )))
                        .expect("valid catalogue response")
                }
                Err(refusal) => Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from(
                        serde_json::to_vec(&refusal).expect("refusal serialises"),
                    )))
                    .expect("valid refusal response"),
            });
        }
        if request.method() != http::Method::POST || request.uri().path() != "/call" {
            return full(refusal_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "usage",
                Category::Usage,
                "unknown operation interface route",
            ));
        }
        let bytes = match Limited::new(
            request
                .into_body()
                .map_err(|error| std::io::Error::other(error.to_string())),
            limit,
        )
        .collect()
        .await
        {
            Ok(bytes) => bytes.to_bytes(),
            Err(_) => {
                return full(refusal_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "invalid_input",
                    Category::InvalidInput,
                    "request exceeds bounds.message_bytes",
                ));
            }
        };
        let invocation: Invocation = match serde_json::from_slice(&bytes) {
            Ok(invocation) => invocation,
            Err(_) => {
                return full(refusal_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_input",
                    Category::InvalidInput,
                    "malformed operation invocation",
                ));
            }
        };
        if let Some(response) = self.scope_challenge(&auth, &invocation.name) {
            return full(response);
        }
        let cancel = Cancellation::new();
        let (sender, receiver) = mpsc::channel(1);
        let records = {
            let sender = sender.clone();
            let delivery = Arc::new(Semaphore::new(1));
            let registry = self.acknowledgements.clone();
            let expected = auth.clone();
            let stopped = cancel.clone();
            RecordSink::new(move |record| {
                let sender = sender.clone();
                let registry = registry.clone();
                let expected = expected.clone();
                let stopped = stopped.clone();
                let delivery = delivery.clone();
                async move {
                    let _delivery = tokio::select! {biased; _=stopped.cancelled()=>return Err(delivery_error()),permit=delivery.acquire_owned()=>permit.expect("private record semaphore")};
                    let ticket = uuid::Uuid::new_v4().to_string();
                    let (accepted, received) = oneshot::channel();
                    registry
                        .lock()
                        .expect("acknowledgement registry")
                        .pending
                        .insert(
                            ticket.clone(),
                            PendingRecord {
                                auth: expected,
                                received: accepted,
                            },
                        );
                    let _pending = PendingAcknowledgement {
                        ticket: ticket.clone(),
                        registry,
                    };
                    send(&sender, "record", record, limit, Some(&ticket)).await?;
                    tokio::select! { biased; _=stopped.cancelled()=>Err(delivery_error()), accepted=received=>accepted.map_err(|_|delivery_error()) }
                }
            })
        };
        let set = self.set.clone();
        let selection = self.selection.clone();
        let execution = self.executing.clone();
        let task_cancel = cancel.clone();
        tokio::spawn(async move {
            let _outstanding = outstanding;
            let permit = tokio::select! { biased; _ = task_cancel.cancelled() => return, permit = execution.acquire_owned() => permit };
            let Ok(_permit) = permit else {
                return;
            };
            let result = set
                .call(
                    &selection,
                    auth,
                    &invocation.name,
                    invocation.input,
                    records,
                    task_cancel,
                    invocation.preview,
                )
                .await;
            let outcome = result.is_ok();
            let (kind, data) = match result {
                Ok(outcome) => (
                    "outcome",
                    serde_json::to_value(outcome).expect("outcome serialises"),
                ),
                Err(refusal) => (
                    "refusal",
                    serde_json::to_value(refusal).expect("refusal serialises"),
                ),
            };
            if let Err(refusal) = send(&sender, kind, data, limit, None).await
                && outcome
                && refusal.code == "result_too_large"
            {
                let data = serde_json::to_value(refusal).expect("refusal serialises");
                let _ = send(&sender, "refusal", data, limit, None).await;
            }
        });
        Response::builder()
            .header(header::CONTENT_TYPE, "application/x-ndjson")
            .body(
                Records {
                    receiver,
                    acknowledgement: None,
                    cancel,
                }
                .boxed(),
            )
            .expect("valid operation stream")
    }
}
