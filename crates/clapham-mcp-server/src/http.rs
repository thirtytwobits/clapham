#[path = "calls_http.rs"]
mod calls;
use std::{
    convert::Infallible,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use clapham::{Category, ComposedSet, Evidence, OAuthContext, Refusal, Selection};
use http::{Request, Response, StatusCode, header};
use http_body::Body;
use http_body_util::{BodyExt, Empty, Full, Limited, combinators::BoxBody};
use rmcp::{
    ErrorData,
    model::{CustomRequest, CustomResult, ProtocolVersion, ServerCapabilities, ServerConfig},
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{ACKNOWLEDGE_REQUEST, Server, register_resources, validate_resources};

/// An opaque HTTP Authorization value. Debug output never prints its contents.
///
/// Missing Authorization is represented by [`Default`]; only the application's
/// authenticator decides whether that credential establishes authority.
#[derive(Clone, Default)]
pub struct Credential {
    authorization: Option<String>,
}

impl Credential {
    /// A credential containing the complete Authorization field value, unchanged.
    pub fn new(authorization: impl Into<String>) -> Self {
        Self {
            authorization: Some(authorization.into()),
        }
    }

    /// The complete Authorization field value, or `None` when absent.
    pub fn authorization(&self) -> Option<&str> {
        self.authorization.as_deref()
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credential")
            .field("present", &self.authorization.is_some())
            .finish()
    }
}

/// Application policy mapping one request's credential to verified OAuth authority.
///
/// Returning `None` refuses the HTTP request before reading its body or running
/// any operation. Identity is never retained from an earlier HTTP request.
pub trait Authenticator: Send + Sync + 'static {
    /// Verify the credential using trusted OAuth middleware or introspection.
    fn authenticate<'a>(&'a self, credential: &'a Credential) -> AuthenticationFuture<'a>;
}

/// Asynchronous OAuth token-verification result.
pub type AuthenticationFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = Option<OAuthContext>> + Send + 'a>>;

impl<F> Authenticator for F
where
    F: Fn(&Credential) -> Option<OAuthContext> + Send + Sync + 'static,
{
    fn authenticate<'a>(&'a self, credential: &'a Credential) -> AuthenticationFuture<'a> {
        Box::pin(std::future::ready(self(credential)))
    }
}

/// Stateless MCP Streamable HTTP over a selected set, authenticated per request.
///
/// Implements the Tower service trait for HTTP servers and routers. The application
/// owns listening, TLS and shutdown. Clones share the bounded request queue and gate;
/// each request establishes its own verified OAuth authority.
#[derive(Clone)]
pub struct HttpServer {
    set: ComposedSet,
    selection: Selection,
    discovery: Selection,
    resources: std::collections::BTreeMap<String, String>,
    authenticator: Arc<dyn Authenticator>,
    outstanding: Arc<Semaphore>,
    authenticating: Arc<Semaphore>,
    acknowledgements: Arc<std::sync::Mutex<calls::Acknowledgements>>,
    deliveries: Arc<crate::records::Deliveries>,
    executing: Arc<Semaphore>,
    authentication_challenge: Option<http::HeaderValue>,
    metadata_path: Option<String>,
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
}

impl fmt::Debug for HttpServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpServer")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl HttpServer {
    /// Expose all operations, using the supplied authenticator on every HTTP request.
    /// The set must specify its OAuth resource through configuration or compose options.
    pub fn new(set: ComposedSet, authenticator: impl Authenticator) -> Result<Self, Refusal> {
        let selection = set.all();
        validate_resources(&selection, &Default::default())?;
        let (metadata_path, challenge) = match set.oauth_resource() {
            Some(resource) => {
                let mut url = reqwest::Url::parse(&resource.audience).map_err(|_| {
                    crate::configuration("OAuth resource must be an absolute HTTP URI")
                })?;
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
                    return Err(crate::configuration("invalid OAuth resource URI"));
                }
                let path = if url.path() == "/" { "" } else { url.path() };
                let path = format!("/.well-known/oauth-protected-resource{path}");
                url.set_path(&path);
                (Some(path), format!("Bearer resource_metadata=\"{url}\""))
            }
            None => {
                return Err(crate::configuration(
                    "HTTP requires an OAuth resource binding; use Configuration or ComposeOptions::oauth",
                ));
            }
        };
        let challenge = challenge
            .parse()
            .map_err(|_| crate::configuration("invalid OAuth metadata URI"))?;
        let bounds = set.bounds();
        let config = StreamableHttpServerConfig::default();
        Ok(Self {
            outstanding: Arc::new(Semaphore::new(bounds.in_flight + bounds.waiting)),
            authenticating: Arc::new(Semaphore::new(bounds.in_flight + bounds.waiting)),
            acknowledgements: Default::default(),
            deliveries: Default::default(),
            executing: Arc::new(Semaphore::new(bounds.in_flight)),
            authentication_challenge: Some(challenge),
            metadata_path,
            set,
            discovery: selection.clone(),
            selection,
            resources: Default::default(),
            authenticator: Arc::new(authenticator),
            allowed_hosts: config.allowed_hosts,
            allowed_origins: Vec::new(),
        })
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

    /// Replace the exposed selection, retaining the internal describe operation.
    pub fn select<I, S>(mut self, names: I) -> Result<Self, Refusal>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.selection = self.set.select(names)?;
        validate_resources(&self.selection, &self.resources)?;
        self.discovery = self.set.select(
            self.selection
                .operations()
                .map(|op| op.name.as_str())
                .chain(["clapham.describe"]),
        )?;
        Ok(self)
    }

    fn scope_challenge(&self, auth: &OAuthContext, name: &str) -> Option<Response<Full<Bytes>>> {
        let operation = self
            .set
            .get(name)
            .filter(|_| self.selection.contains(name))?;
        let required = &operation.descriptor().scopes;
        if required.iter().all(|scope| auth.has_scope(scope)) {
            return None;
        }
        let mut refusal = Refusal::not_dispatched(
            "insufficient_scope",
            Category::Refused,
            "the grant lacks required scopes",
        );
        refusal.details = Some(serde_json::json!({"required_scopes":required}));
        let mut response = Response::builder()
            .status(StatusCode::FORBIDDEN)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(
                serde_json::to_vec(&refusal).expect("refusal serialises"),
            )))
            .expect("valid scope refusal");
        let challenge = format!(
            "Bearer error=\"insufficient_scope\", scope=\"{}\"",
            required.join(" ")
        )
        .parse()
        .expect("composition checks scope syntax");
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, challenge);
        Some(response)
    }

    /// Allowed HTTP hosts or authorities. Defaults to rmcp's loopback host list.
    pub fn with_allowed_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Allowed browser origins. By default, every present Origin field is refused;
    /// clients without an Origin field remain permitted by this transport check.
    pub fn with_allowed_origins<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_origins = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Set the WWW-Authenticate challenge on unmapped credentials. No challenge
    /// is supplied by default; the host chooses its scheme and parameters.
    pub fn with_authentication_challenge(mut self, challenge: http::HeaderValue) -> Self {
        self.authentication_challenge = Some(challenge);
        self
    }

    fn transport_config(&self) -> StreamableHttpServerConfig {
        stateless_config(self.set.bounds().message_bytes)
            .with_allowed_hosts(self.allowed_hosts.clone())
            .with_allowed_origins(self.allowed_origins.clone())
            .enforce_origin_validation()
    }

    /// Handle one request using rmcp's stateless server transport. HTTP admission
    /// happens before body consumption.
    ///
    /// Unmapped credentials receive 401, malformed credentials 400, and an exhausted
    /// queue 503. Body limits and MCP header validation are enforced by rmcp. A JSON
    /// reply is bounded as a whole. An SSE reply streams as rmcp writes it, each event
    /// bounded, and holds the request's queue and execution permits until it ends. A JSON
    /// reply that cannot fit without changing an existing refusal produces HTTP 500; an
    /// SSE reply closes instead. Record acknowledgements are admitted outside the request
    /// queue and execution permits, and answer nothing else.
    pub async fn handle<B>(&self, request: Request<B>) -> Response<BoxBody<Bytes, Infallible>>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: fmt::Display,
    {
        let limit = self.set.bounds().message_bytes;
        let config = self.transport_config();
        // rmcp 3.6.0 keeps its Host/Origin validator private. An OPTIONS probe
        // follows that exact policy and stops at 405, without parsing a body or
        // constructing a handler. Reusing it avoids a second authority/origin parser.
        let validator = StreamableHttpService::<Server, _>::new(
            || Err(std::io::Error::other("HTTP policy probe cannot dispatch")),
            Arc::new(LocalSessionManager::default()),
            config.clone(),
        );
        let mut probe = Request::new(Empty::<Bytes>::new());
        *probe.method_mut() = http::Method::OPTIONS;
        *probe.uri_mut() = request.uri().clone();
        *probe.version_mut() = request.version();
        *probe.headers_mut() = request.headers().clone();
        let checked = validator.handle(probe).await;
        if checked.status() != StatusCode::METHOD_NOT_ALLOWED {
            return calls::full(bounded_response(checked, limit, false).await);
        }

        let mut authorization = request.headers().get_all(header::AUTHORIZATION).iter();
        let credential = match (authorization.next(), authorization.next()) {
            (None, None) => Credential::default(),
            (Some(value), None) => match value.to_str() {
                Ok(value) => Credential::new(value),
                Err(_) => {
                    return calls::full(refusal_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        Category::InvalidInput,
                        "the Authorization field is malformed",
                    ));
                }
            },
            _ => {
                return calls::full(refusal_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_credential",
                    Category::InvalidInput,
                    "multiple Authorization fields are not permitted",
                ));
            }
        };
        let method = request
            .headers()
            .get("mcp-method")
            .and_then(|value| value.to_str().ok());
        let acknowledgement = method == Some(ACKNOWLEDGE_REQUEST);
        let tool_call = method == Some("tools/call");
        // A streaming call holding every permit waits for its acknowledgements.
        let outstanding = if acknowledgement {
            None
        } else {
            let Ok(outstanding) = self.outstanding.clone().try_acquire_owned() else {
                return calls::full(refusal_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "server_busy",
                    Category::Unavailable,
                    "the HTTP request queue is full",
                ));
            };
            Some(outstanding)
        };
        let Ok(authentication) = self.authenticating.try_acquire() else {
            return calls::full(refusal_response(
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
                "the credential was not authenticated",
            );
            if let Some(challenge) = &self.authentication_challenge {
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, challenge.clone());
            }
            return calls::full(response);
        };
        if acknowledgement {
            // The authentication permit bounds concurrent acknowledgements.
            let acknowledging = Acknowledging {
                deliveries: self.deliveries.clone(),
                auth,
            };
            let service = StreamableHttpService::new(
                move || Ok(acknowledging.clone()),
                Arc::new(LocalSessionManager::default()),
                config,
            );
            let response = bounded_response(service.handle(request).await, limit, false).await;
            drop(authentication);
            return calls::full(response);
        }
        drop(authentication);
        if tool_call
            && let Some(name) = request
                .headers()
                .get("mcp-name")
                .and_then(|value| value.to_str().ok())
            && let Some(response) = self.scope_challenge(&auth, name)
        {
            return calls::full(response);
        }
        let executing = self
            .executing
            .clone()
            .acquire_owned()
            .await
            .expect("private semaphore remains open");
        let failure = Arc::new(AtomicBool::new(false));
        let server = Server {
            set: self.set.clone(),
            selection: self.selection.clone(),
            discovery: self.discovery.clone(),
            auth,
            resources: self.resources.clone(),
            // The outer HTTP execution permit covers this entire request.
            slots: None,
            deliveries: self.deliveries.clone(),
            http_failure: Some(failure.clone()),
        };
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let response = service.handle(request).await;
        let streams = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if streams {
            return response.map(|body| {
                Events {
                    body,
                    pending: Vec::new(),
                    scanned: 0,
                    limit,
                    failure,
                    closed: false,
                    _permits: (outstanding, executing),
                }
                .boxed()
            });
        }
        calls::full(bounded_response(response, limit, failure.load(Ordering::Acquire)).await)
    }
}

/// The handler behind an acknowledgement admitted outside the request limits. It
/// answers record acknowledgements and nothing else.
#[derive(Clone)]
struct Acknowledging {
    deliveries: Arc<crate::records::Deliveries>,
    auth: OAuthContext,
}

impl rmcp::ServerHandler for Acknowledging {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::default())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        crate::records::acknowledge(&self.deliveries, &self.auth, request)
    }
}

// SSE field names, separators and an event id around one JSON-RPC message.
const EVENT_FRAMING: usize = 256;

/// An SSE reply passed on event by event as rmcp writes it. An event that cannot fit
/// `bounds.message_bytes` with its framing, or a reply whose budget failed, closes the
/// stream. The request's permits live as long as the stream.
struct Events {
    body: BoxBody<Bytes, Infallible>,
    pending: Vec<u8>,
    scanned: usize,
    limit: usize,
    failure: Arc<AtomicBool>,
    closed: bool,
    _permits: (Option<OwnedSemaphorePermit>, OwnedSemaphorePermit),
}

impl Body for Events {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        let events = &mut *self;
        loop {
            if events.closed {
                return Poll::Ready(None);
            }
            let start = events.scanned.saturating_sub(1);
            if let Some(end) = events.pending[start..]
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|offset| start + offset + 2)
            {
                if end > events.limit + EVENT_FRAMING || events.failure.load(Ordering::Acquire) {
                    events.closed = true;
                    continue;
                }
                let event: Vec<u8> = events.pending.drain(..end).collect();
                events.scanned = 0;
                return Poll::Ready(Some(Ok(http_body::Frame::data(Bytes::from(event)))));
            }
            events.scanned = events.pending.len();
            if events.pending.len() > events.limit + EVENT_FRAMING {
                events.closed = true;
                continue;
            }
            match std::pin::Pin::new(&mut events.body).poll_frame(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        events.pending.extend_from_slice(&data);
                    }
                }
                Poll::Ready(Some(Err(never))) => match never {},
                Poll::Ready(None) => events.closed = true,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<B> tower_service::Service<Request<B>> for HttpServer
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: fmt::Display,
{
    type Response = Response<BoxBody<Bytes, Infallible>>;
    type Error = Infallible;
    type Future =
        std::pin::Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn call(&mut self, request: Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { Ok(service.handle(request).await) })
    }
}

/// rmcp's stateless Streamable HTTP transport as every `HttpServer` request uses it,
/// before Host and Origin policy.
pub(crate) fn stateless_config(message_bytes: usize) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .with_max_request_body_bytes(message_bytes)
        .with_stateless_protocol_metadata_required(true)
}

fn refusal_response(
    status: StatusCode,
    code: &str,
    category: Category,
    message: &str,
) -> Response<Full<Bytes>> {
    let refusal = if status == StatusCode::INTERNAL_SERVER_ERROR {
        Refusal::new(code, category, message).with_evidence(Evidence::Uncertain)
    } else {
        Refusal::not_dispatched(code, category, message)
    };
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(
            serde_json::to_vec(&refusal).expect("refusals serialize"),
        )))
        .expect("static HTTP response is valid")
}

async fn bounded_response(
    response: Response<BoxBody<Bytes, Infallible>>,
    limit: usize,
    failed: bool,
) -> Response<Full<Bytes>> {
    let (mut parts, body) = response.into_parts();
    // A failed reply budget is an HTTP delivery failure, rather than an operation refusal.
    if failed {
        parts.status = StatusCode::INTERNAL_SERVER_ERROR;
    }
    match Limited::new(body, limit).collect().await {
        Ok(body) => Response::from_parts(parts, Full::new(body.to_bytes())),
        Err(_) => refusal_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "reply_unavailable",
            Category::Unavailable,
            "the HTTP response exceeds bounds.message_bytes; operation effects are uncertain",
        ),
    }
}
