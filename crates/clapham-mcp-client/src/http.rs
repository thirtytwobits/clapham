//! Copyright (c) Scott A Dixon
//! Streamable HTTP with bounded response bodies and disabled POST recovery.
use crate::{
    Clock, McpConfig, McpConnection, McpError,
    clock::{deadline_after, timeout_at},
    connection::State,
    wire,
};
use futures_util::{StreamExt, stream::BoxStream};
use http::{HeaderName, HeaderValue};
use rmcp::{
    RoleClient,
    model::{ClientJsonRpcMessage, ClientRequest, ServerJsonRpcMessage},
    transport::{Transport, common::client_side_sse::SseRetryPolicy, streamable_http_client::*},
};
use sse_stream::{Error as SseError, Sse, SseStream};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

// SSE field names, separators and an event id around one JSON-RPC message.
const EVENT_FRAMING: usize = 256;

/// Endpoint and host-resolved bearer credential. Debug formatting intentionally omits both.
pub struct McpHttpOptions {
    /// Endpoint approved by the host, without embedded user credentials or a fragment.
    pub endpoint: String,
    /// Host-resolved bearer token; never included in debug output.
    pub bearer_token: Option<String>,
}
impl McpHttpOptions {
    /// Connect without a bearer token unless the host supplies one.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token: None,
        }
    }
}
impl std::fmt::Debug for McpHttpOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHttpOptions").finish_non_exhaustive()
    }
}
#[derive(Clone)]
struct BoundedHttp {
    client: reqwest::Client,
    limit: usize,
    timeout: Duration,
    clock: Arc<dyn Clock>,
    state: Arc<State>,
}
/// An HTTP failure, with the structured refusal a Clapham server gave for it. Only a caller
/// that reads a server's refusals unchanged, such as the CLI's direct calls, receives it.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub(crate) struct Fault {
    pub(crate) error: McpError,
    pub(crate) refusal: Option<Box<clapham::Refusal>>,
}
impl From<McpError> for Fault {
    fn from(error: McpError) -> Self {
        Self {
            error,
            refusal: None,
        }
    }
}
pub(crate) type HttpError = StreamableHttpError<Fault>;
fn error(e: McpError) -> HttpError {
    StreamableHttpError::Client(e.into())
}
/// The structured refusal in a bounded response body, if there is one.
async fn server_refusal(
    mut response: reqwest::Response,
    limit: usize,
) -> Option<Box<clapham::Refusal>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok().map(Box::new)
}
fn media_type(response: &reqwest::Response, expected: &str) -> bool {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}
impl BoundedHttp {
    fn fail(&self, fault: McpError) -> HttpError {
        self.state.fail(fault.clone());
        error(fault)
    }
    fn new(config: &McpConfig, state: Arc<State>) -> Result<Self, McpError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| McpError::Configuration)?;
        Ok(Self {
            client,
            limit: config.bounds.message_bytes,
            timeout: config.operation_timeout,
            clock: config.clock.clone(),
            state,
        })
    }
    /// The deadline of a request, including its response body, on the connection's clock. A
    /// tool call is bounded by its invocation deadline or its caller, so it has none of its own.
    fn deadline(&self, message: Option<&ClientJsonRpcMessage>) -> Option<Instant> {
        let call = matches!(message, Some(ClientJsonRpcMessage::Request(request))
            if matches!(request.request, ClientRequest::CallToolRequest(_)));
        (!call).then(|| deadline_after(self.clock.as_ref(), self.timeout))
    }
    /// Runs `request` until `deadline`, when there is one.
    async fn within<T>(
        &self,
        deadline: Option<Instant>,
        request: impl Future<Output = Result<T, HttpError>>,
    ) -> Result<T, HttpError> {
        match deadline {
            Some(deadline) => timeout_at(self.clock.as_ref(), deadline, request)
                .await
                .unwrap_or_else(|_| Err(error(McpError::Timeout))),
            None => request.await,
        }
    }
    fn request(
        &self,
        method: reqwest::Method,
        uri: &str,
        session: Option<&str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .request(method, uri)
            .header("accept", "application/json, text/event-stream");
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        if let Some(auth) = auth {
            request = request.bearer_auth(auth);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        request
    }
    /// The POST carrying an encoded message.
    fn post(
        &self,
        uri: &str,
        session: Option<&str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        body: Vec<u8>,
    ) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, uri, session, auth, headers)
            .header("content-type", "application/json")
            .body(body)
    }
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response, HttpError> {
        let response = request
            .send()
            .await
            .map_err(|_| error(McpError::Transport))?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        // An unaccepted credential ends the connection; a scope challenge or a full queue
        // refuses this request alone, before dispatch.
        let error = match status {
            reqwest::StatusCode::UNAUTHORIZED => McpError::Unauthenticated,
            reqwest::StatusCode::FORBIDDEN => McpError::Forbidden,
            reqwest::StatusCode::SERVICE_UNAVAILABLE => McpError::Busy,
            _ => McpError::Transport,
        };
        if error == McpError::Unauthenticated {
            self.state.fail(error.clone());
        }
        Err(StreamableHttpError::Client(Fault {
            error,
            refusal: server_refusal(response, self.limit).await,
        }))
    }
    /// The SSE events of `response`. Its body fails with a timeout at `deadline`, when there is one.
    fn stream(
        &self,
        response: reqwest::Response,
        deadline: Option<Instant>,
    ) -> BoxStream<'static, Result<Sse, SseError>> {
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|_| McpError::Transport));
        let body = match deadline {
            None => body.boxed(),
            Some(deadline) => {
                let timer = self.clock.sleep_until(deadline);
                futures_util::stream::unfold(Some((body.boxed(), timer)), |held| async move {
                    let (mut body, mut timer) = held?;
                    tokio::select! {
                        biased;
                        chunk = body.next() => chunk.map(|chunk| (chunk, Some((body, timer)))),
                        () = &mut timer => Some((Err(McpError::Timeout), None)),
                    }
                })
                .boxed()
            }
        };
        let state = self.state.clone();
        // A response may carry one event beyond the notifications already admitted, so
        // keep-alive comments cannot grow it while records stream. Notifications draw on the
        // connection's budgets.
        let allowance = Arc::new(AtomicUsize::new(self.limit + EVENT_FRAMING));
        let admitted = allowance.clone();
        // Each event is bounded before parsing, and the parser admits it before the next
        // event's bytes are counted, however the transport chunks them.
        let mut events = EventBoundaries::new(self.limit + EVENT_FRAMING);
        let pieces = body.flat_map(move |chunk| {
            futures_util::stream::iter(match chunk {
                Ok(chunk) => events.split(chunk),
                Err(error) => vec![Err(error)],
            })
        });
        let bytes = pieces.scan(0usize, move |count, chunk| {
            let result = chunk.and_then(|chunk| {
                *count = count.saturating_add(chunk.len());
                if *count > admitted.load(Ordering::SeqCst) {
                    Err(McpError::Limit)
                } else {
                    Ok(chunk)
                }
            });
            if let Err(error) = &result {
                state.fail(error.clone());
            }
            std::future::ready(Some(result))
        });
        let state = self.state.clone();
        SseStream::from_bytes_stream(bytes)
            .map(move |event| {
                let mut event = event?;
                // Preserve raw catalogue records through the SDK's untagged result parser.
                if let Some(data) = &mut event.data
                    && let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data)
                {
                    if value.get("method").is_some() {
                        let paced = value["method"] == crate::records::RECORD_NOTIFICATION
                            && value["params"]["token"]
                                .as_str()
                                .is_some_and(|token| state.routes.awaits(token));
                        if !paced && let Err(error) = state.charge_incoming_bytes(data.len()) {
                            state.fail(error.clone());
                            return Err(SseError::Body(Box::new(error)));
                        }
                        allowance.fetch_add(data.len() + EVENT_FRAMING, Ordering::SeqCst);
                    }
                    state.requests.prepare(&mut value);
                    *data = value.to_string();
                }
                Ok(event)
            })
            .boxed()
    }
}

/// Splits an SSE byte stream after each blank line, the end of an event, and refuses an
/// event longer than `limit` bytes.
struct EventBoundaries {
    line: usize,
    event: usize,
    limit: usize,
}
impl EventBoundaries {
    fn new(limit: usize) -> Self {
        Self {
            line: 0,
            event: 0,
            limit,
        }
    }
    fn split(&mut self, chunk: bytes::Bytes) -> Vec<Result<bytes::Bytes, McpError>> {
        let mut pieces = Vec::new();
        let mut start = 0;
        for (index, byte) in chunk.iter().enumerate() {
            self.event += 1;
            if self.event > self.limit {
                pieces.push(Err(McpError::Limit));
                return pieces;
            }
            match byte {
                b'\n' => {
                    if self.line == 0 {
                        pieces.push(Ok(chunk.slice(start..=index)));
                        start = index + 1;
                        self.event = 0;
                    }
                    self.line = 0;
                }
                b'\r' => {}
                _ => self.line += 1,
            }
        }
        if start < chunk.len() {
            pieces.push(Ok(chunk.slice(start..)));
        }
        pieces
    }
}
impl StreamableHttpClient for BoundedHttp {
    type Error = Fault;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let sending = self
            .state
            .requests
            .begin(&message)
            .map_err(|fault| self.fail(fault))?;
        let deadline = self.deadline(Some(&message));
        let result = self
            .within(deadline, async {
                let body = wire::encode(&message, self.limit).map_err(error)?;
                let response = self
                    .send(self.post(&uri, session_id.as_deref(), auth, headers, body))
                    .await?;
                if matches!(response.status().as_u16(), 202 | 204) {
                    return Ok(StreamableHttpPostResponse::Accepted);
                }
                let session = response
                    .headers()
                    .get("mcp-session-id")
                    .map(|h| h.to_str().map(str::to_owned))
                    .transpose()
                    .map_err(|_| self.fail(McpError::Protocol))?;
                if media_type(&response, "text/event-stream") {
                    return Ok(StreamableHttpPostResponse::Sse(
                        self.stream(response, deadline),
                        session,
                    ));
                }
                if !media_type(&response, "application/json") {
                    return Err(self.fail(McpError::Protocol));
                }
                let mut bytes = Vec::new();
                let mut stream = response.bytes_stream();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| error(McpError::Transport))?;
                    if chunk.len() > self.limit.saturating_sub(bytes.len()) {
                        self.state.fail(McpError::Limit);
                        return Err(error(McpError::Limit));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let message =
                    wire::decode(&bytes, &self.state.requests).map_err(|fault| self.fail(fault))?;
                if let Err(fault) = self.state.observe_bytes(&message, bytes.len()) {
                    self.state.fail(fault.clone());
                    return Err(error(fault));
                }
                Ok(StreamableHttpPostResponse::Json(message, session))
            })
            .await;
        if result.is_ok() {
            sending.sent();
        }
        result
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        let request = self.request(reqwest::Method::DELETE, &uri, Some(&session), auth, headers);
        self.within(self.deadline(None), self.send(request)).await?;
        Ok(())
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError> {
        let mut request = self.request(
            reqwest::Method::GET,
            &uri,
            session.as_deref(),
            auth,
            headers,
        );
        if let Some(last) = last {
            request = request.header("last-event-id", last);
        }
        // The headers have a deadline; the persistent stream does not.
        let response = self
            .within(self.deadline(None), async {
                request.send().await.map_err(|_| error(McpError::Transport))
            })
            .await?;
        if response.status().as_u16() == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(error(McpError::Transport));
        }
        if !media_type(&response, "text/event-stream") {
            return Err(self.fail(McpError::Protocol));
        }
        Ok(self.stream(response, None))
    }
}
#[derive(Debug)]
struct NoRetry;
impl SseRetryPolicy for NoRetry {
    fn retry(&self, _: usize) -> Option<Duration> {
        None
    }
}
struct Guard<T> {
    inner: T,
    state: Arc<State>,
    notifications: usize,
}
impl<T: Transport<RoleClient> + 'static> Transport<RoleClient> for Guard<T> {
    type Error = T::Error;
    fn send(
        &mut self,
        message: ClientJsonRpcMessage,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(message)
    }
    async fn receive(&mut self) -> Option<ServerJsonRpcMessage> {
        let message = self.inner.receive().await?;
        if let Err(error) = self.state.observe(&message, self.notifications) {
            self.state.fail(error);
            return None;
        }
        Some(message)
    }
    async fn close(&mut self) -> Result<(), Self::Error> {
        self.inner.close().await
    }
}
impl McpConnection {
    /// Connect a host-approved endpoint. Redirects, POST retries and session reinitialisation are disabled.
    pub async fn http(options: McpHttpOptions, config: McpConfig) -> Result<Self, McpError> {
        config.validate()?;
        validate_endpoint(&options.endpoint)?;
        let state = State::new();
        let backend = BoundedHttp::new(&config, state.clone())?;
        let mut transport_config = StreamableHttpClientTransportConfig::with_uri(options.endpoint);
        transport_config.auth_header = options.bearer_token;
        transport_config.reinit_on_expired_session = false;
        transport_config.retry_config = Arc::new(NoRetry);
        // Each operation in flight may also acknowledge a record.
        transport_config.max_concurrent_requests = 2 * config.bounds.in_flight;
        transport_config.channel_buffer_capacity = 2 * config.bounds.in_flight;
        transport_config.max_sse_event_size = config.bounds.message_bytes;
        // The backend bounds each control POST by the operation deadline on the configured clock.
        transport_config.control_request_timeout = Duration::MAX;
        let transport = Guard {
            inner: StreamableHttpClientTransport::with_client(backend, transport_config),
            state: state.clone(),
            notifications: config.bounds.notifications,
        };
        Self::start(transport, config, state).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_clock as clock;
    use axum::{
        Router,
        response::{IntoResponse, Response},
        routing::get,
    };

    async fn padded_notification(
        axum::extract::Path(format): axum::extract::Path<String>,
    ) -> Response {
        let payload = format!(
            r#"{{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}}{}"#,
            " ".repeat(512)
        );
        if format == "sse" {
            (
                [("content-type", "text/event-stream")],
                format!("data: {payload}\n\n"),
            )
                .into_response()
        } else {
            ([("content-type", "application/json")], payload).into_response()
        }
    }

    async fn server() -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new().route(
            "/notification/{format}",
            axum::routing::post(padded_notification),
        );
        serve(app).await
    }

    async fn serve(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async {
            axum::serve(listener, app).await.unwrap();
        });
        (endpoint, task)
    }

    fn backend() -> (BoundedHttp, clock::ManualClock) {
        let clock = clock::ManualClock::new();
        let mut config = McpConfig::new("remote", "scope");
        config.operation_timeout = Duration::from_secs(3600);
        config.clock = Arc::new(clock.clone());
        (BoundedHttp::new(&config, State::new()).unwrap(), clock)
    }

    #[tokio::test]
    async fn persistent_sse_survives_operation_deadlines() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let receiver = Arc::new(std::sync::Mutex::new(Some(rx)));
        let app = Router::new().route(
            "/events",
            get(move || {
                let rx = receiver.lock().unwrap().take().unwrap();
                async move {
                    let events = futures_util::stream::unfold(rx, |mut rx| async move {
                        rx.recv().await.map(|text: String| {
                            (
                                Ok::<_, std::convert::Infallible>(
                                    axum::response::sse::Event::default().data(text),
                                ),
                                rx,
                            )
                        })
                    });
                    let mut response = axum::response::Sse::new(events).into_response();
                    response.headers_mut().insert(
                        "content-type",
                        "Text/Event-Stream ; charset=utf-8".parse().unwrap(),
                    );
                    response
                }
            }),
        );
        let (endpoint, task) = serve(app).await;
        tx.send("first".into()).unwrap();
        let (backend, clock) = backend();
        let mut stream = backend
            .get_stream(
                format!("{endpoint}/events").into(),
                None,
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap().data.as_deref(),
            Some("first")
        );
        clock.advance(backend.timeout * 3);
        tx.send("after deadline".into()).unwrap();
        assert_eq!(
            stream.next().await.unwrap().unwrap().data.as_deref(),
            Some("after deadline")
        );
        assert_eq!(backend.state.check(), Ok(()));
        task.abort();
    }

    #[test]
    fn a_tool_call_post_has_no_request_deadline() {
        let (backend, clock) = backend();
        let deadline = |message: serde_json::Value| {
            backend.deadline(Some(&serde_json::from_value(message).unwrap()))
        };
        let call = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"export","arguments":{}}});
        let ping = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"ping"});
        // The call is bounded by its invocation deadline or its caller instead.
        assert_eq!(deadline(call), None);
        assert_eq!(deadline(ping), Some(clock.now() + backend.timeout));
        assert_eq!(backend.deadline(None), Some(clock.now() + backend.timeout));
    }

    #[tokio::test]
    async fn persistent_sse_keeps_the_whole_response_byte_bound() {
        // Every individual event is small; keep-alive comments still consume body bytes.
        let body = ": keep-alive\n\n".repeat(1024);
        let app = Router::new().route(
            "/volume",
            get(move || {
                let body = body.clone();
                async move { ([("content-type", "text/event-stream")], body) }
            }),
        );
        let (endpoint, task) = serve(app).await;
        let (mut backend, _clock) = backend();
        backend.limit = 8192;
        let mut stream = backend
            .get_stream(
                format!("{endpoint}/volume").into(),
                None,
                None,
                None,
                HashMap::new(),
            )
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(backend.state.check(), Err(McpError::Limit));
        task.abort();
    }

    #[tokio::test]
    async fn notifications_in_one_chunk_are_each_admitted_before_the_next() {
        let event = |message: serde_json::Value| format!("data: {message}\n\n");
        let notification = |sequence: u64| {
            event(
                serde_json::json!({"jsonrpc":"2.0","method":"example.test/record",
                "params":{"sequence":sequence,"pad":"x".repeat(5000)}}),
            )
        };
        // Each event fits the bound; together they exceed it before any is parsed.
        let body = notification(1)
            + &notification(2)
            + &event(serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}));
        let response = http::Response::builder()
            .header("content-type", "text/event-stream")
            .body(body)
            .unwrap();
        let (mut backend, _clock) = backend();
        backend.limit = 8192;
        let events: Vec<_> = backend
            .stream(reqwest::Response::from(response), None)
            .collect()
            .await;
        assert_eq!(events.len(), 3);
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(backend.state.check(), Ok(()));
    }

    #[tokio::test]
    async fn admitted_notifications_cannot_fund_an_oversized_event() {
        let event = |message: serde_json::Value| format!("data: {message}\n\n");
        let small = event(serde_json::json!({"jsonrpc":"2.0","method":"example.test/record"}));
        let oversized = event(
            serde_json::json!({"jsonrpc":"2.0","method":"example.test/record",
            "params":{"pad":"x".repeat(20_000)}}),
        );
        let response = http::Response::builder()
            .header("content-type", "text/event-stream")
            .body(small.repeat(100) + &oversized)
            .unwrap();
        let (mut backend, _clock) = backend();
        backend.limit = 8192;
        let events: Vec<_> = backend
            .stream(reqwest::Response::from(response), None)
            .collect()
            .await;
        assert_eq!(events.iter().filter(|event| event.is_ok()).count(), 100);
        assert!(events.last().unwrap().is_err());
        assert_eq!(backend.state.check(), Err(McpError::Limit));
    }

    #[tokio::test]
    async fn padded_http_notifications_charge_received_payload_bytes() {
        let (endpoint, task) = server().await;
        for format in ["json", "sse"] {
            let (backend, _clock) = backend();
            backend
                .state
                .charge_incoming_bytes(64 * 1024 * 1024 - 128)
                .unwrap();
            let message =
                serde_json::from_value(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
                    .unwrap();
            let response = backend
                .post_message(
                    format!("{endpoint}/notification/{format}").into(),
                    message,
                    None,
                    None,
                    HashMap::new(),
                )
                .await;
            if format == "sse" {
                let StreamableHttpPostResponse::Sse(mut stream, _) = response.unwrap() else {
                    panic!("expected SSE response");
                };
                assert!(stream.next().await.unwrap().is_err());
            } else {
                assert!(matches!(
                    response,
                    Err(StreamableHttpError::Client(Fault {
                        error: McpError::Limit,
                        ..
                    }))
                ));
            }
            assert_eq!(backend.state.check(), Err(McpError::Limit));
        }
        task.abort();
    }

    #[tokio::test]
    async fn finite_requests_and_get_headers_keep_deadlines() {
        for body in [false, true] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let signal = entered.clone();
            let app = if body {
                Router::new().route(
                    "/held",
                    axum::routing::post(move || {
                        let signal = signal.clone();
                        async move {
                            let chunks = futures_util::stream::once(async move {
                                signal.notify_one();
                                std::future::pending::<Result<String, std::convert::Infallible>>()
                                    .await
                            });
                            let mut response =
                                axum::body::Body::from_stream(chunks).into_response();
                            response
                                .headers_mut()
                                .insert("content-type", "application/json".parse().unwrap());
                            response
                        }
                    }),
                )
            } else {
                Router::new().route(
                    "/held",
                    get(move || {
                        let signal = signal.clone();
                        async move {
                            signal.notify_one();
                            std::future::pending::<Response>().await
                        }
                    }),
                )
            };
            let (endpoint, server) = serve(app).await;
            let (backend, clock) = backend();
            let deadline = backend.timeout;
            let request = tokio::spawn(async move {
                if body {
                    let message = serde_json::from_value(
                        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"ping"}),
                    )
                    .unwrap();
                    backend
                        .post_message(
                            format!("{endpoint}/held").into(),
                            message,
                            None,
                            None,
                            HashMap::new(),
                        )
                        .await
                        .map(|_| ())
                } else {
                    backend
                        .get_stream(
                            format!("{endpoint}/held").into(),
                            None,
                            None,
                            None,
                            HashMap::new(),
                        )
                        .await
                        .map(|_| ())
                }
            });
            entered.notified().await;
            clock.advance(deadline);
            let error = request.await.unwrap().unwrap_err();
            assert!(matches!(
                error,
                StreamableHttpError::Client(Fault {
                    error: McpError::Timeout,
                    ..
                })
            ));
            server.abort();
        }
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), McpError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| McpError::Configuration)?;
    let loopback = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if (url.scheme() != "https" && !(url.scheme() == "http" && loopback))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(McpError::Configuration);
    }
    Ok(())
}
#[cfg(test)]
mod endpoint_tests {
    use super::*;
    #[test]
    fn oauth_endpoints_require_https_or_explicit_loopback_without_embedded_credentials() {
        for endpoint in [
            "https://lists.example.com/mcp",
            "http://localhost:8080/mcp",
            "http://127.0.0.1:8080/mcp",
            "http://[::1]:8080/mcp",
        ] {
            assert!(validate_endpoint(endpoint).is_ok());
        }
        for endpoint in [
            "http://lists.example.com/mcp",
            "ftp://lists.example.com/mcp",
            "https://user:secret@lists.example.com/mcp",
            "https://lists.example.com/mcp?access_token=secret",
            "https://lists.example.com/mcp#fragment",
        ] {
            assert!(validate_endpoint(endpoint).is_err());
        }
    }
}
