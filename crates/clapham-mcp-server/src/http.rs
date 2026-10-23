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
use clapham::{Category, ComposedSet, Evidence, Principal, Refusal, Selection};
use http::{Request, Response, StatusCode, header};
use http_body::Body;
use http_body_util::{BodyExt, Empty, Full, Limited, combinators::BoxBody};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::sync::Semaphore;

use crate::{Server, validate_resources};

/// An opaque HTTP Authorization value. Debug output never prints its contents.
///
/// Missing Authorization is represented by [`Default`]; only the application's
/// authenticator decides whether that credential represents a principal.
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

/// Application policy mapping one request's credential to an authenticated principal.
///
/// Returning `None` refuses the HTTP request before reading its body or running
/// any operation. Identity is never retained from an earlier HTTP request.
pub trait Authenticator: Send + Sync + 'static {
    /// Authenticate the opaque credential supplied on this request.
    fn authenticate(&self, credential: &Credential) -> Option<Principal>;
}

impl<F> Authenticator for F
where
    F: Fn(&Credential) -> Option<Principal> + Send + Sync + 'static,
{
    fn authenticate(&self, credential: &Credential) -> Option<Principal> {
        self(credential)
    }
}

/// Stateless MCP Streamable HTTP over a selected set, authenticated per request.
///
/// Implements the Tower service trait for HTTP servers and routers. The application
/// owns listening, TLS and shutdown. Clones share the bounded request queue and gate;
/// no session or previous principal is used to authorise a request.
#[derive(Clone)]
pub struct HttpServer {
    set: ComposedSet,
    selection: Selection,
    discovery: Selection,
    authenticator: Arc<dyn Authenticator>,
    outstanding: Arc<Semaphore>,
    executing: Arc<Semaphore>,
    authentication_challenge: Option<http::HeaderValue>,
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
    /// Unsupported resource templates refuse before serving, as for [`Server::new`].
    pub fn new(set: ComposedSet, authenticator: impl Authenticator) -> Result<Self, Refusal> {
        let selection = set.all();
        validate_resources(&selection)?;
        let bounds = set.bounds();
        let config = StreamableHttpServerConfig::default();
        Ok(Self {
            outstanding: Arc::new(Semaphore::new(bounds.in_flight + bounds.waiting)),
            executing: Arc::new(Semaphore::new(bounds.in_flight)),
            authentication_challenge: None,
            set,
            discovery: selection.clone(),
            selection,
            authenticator: Arc::new(authenticator),
            allowed_hosts: config.allowed_hosts,
            allowed_origins: Vec::new(),
        })
    }

    /// Replace the exposed selection, retaining the internal describe operation.
    pub fn select<I, S>(mut self, names: I) -> Result<Self, Refusal>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.selection = self.set.select(names)?;
        validate_resources(&self.selection)?;
        self.discovery = self.set.select(
            self.selection
                .operations()
                .map(|op| op.name.as_str())
                .chain(["clapham.describe"]),
        )?;
        Ok(self)
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
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(false)
            .with_json_response(true)
            .with_sse_keep_alive(None)
            .with_sse_retry(None)
            .with_max_request_body_bytes(self.set.bounds().message_bytes)
            .with_stateless_protocol_metadata_required(true)
            .with_allowed_hosts(self.allowed_hosts.clone())
            .with_allowed_origins(self.allowed_origins.clone())
            .enforce_origin_validation()
    }

    /// Handle one request using rmcp's stateless server transport, returning a
    /// bounded JSON response. HTTP admission happens before body consumption.
    ///
    /// Unmapped credentials receive 401, malformed credentials 400, and an exhausted
    /// queue 503. Body limits and MCP header validation are enforced by rmcp. Replies
    /// that cannot fit without changing an existing refusal produce HTTP 500.
    pub async fn handle<B>(&self, request: Request<B>) -> Response<Full<Bytes>>
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
            return bounded_response(checked, limit, false).await;
        }

        let mut authorization = request.headers().get_all(header::AUTHORIZATION).iter();
        let credential = match (authorization.next(), authorization.next()) {
            (None, None) => Credential::default(),
            (Some(value), None) => match value.to_str() {
                Ok(value) => Credential::new(value),
                Err(_) => {
                    return refusal_response(
                        StatusCode::BAD_REQUEST,
                        "invalid_credential",
                        Category::InvalidInput,
                        "the Authorization field is malformed",
                    );
                }
            },
            _ => {
                return refusal_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_credential",
                    Category::InvalidInput,
                    "multiple Authorization fields are not permitted",
                );
            }
        };
        let Some(principal) = self.authenticator.authenticate(&credential) else {
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
            return response;
        };
        let Ok(_outstanding) = self.outstanding.try_acquire() else {
            return refusal_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_busy",
                Category::Unavailable,
                "the HTTP request queue is full",
            );
        };
        let _executing = self
            .executing
            .acquire()
            .await
            .expect("private semaphore remains open");
        let failure = Arc::new(AtomicBool::new(false));
        let server = Server {
            set: self.set.clone(),
            selection: self.selection.clone(),
            discovery: self.discovery.clone(),
            principal,
            // The outer HTTP execution permit covers this entire request.
            slots: None,
            http_failure: Some(failure.clone()),
        };
        let service = StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let response = service.handle(request).await;
        bounded_response(response, limit, failure.load(Ordering::Acquire)).await
    }
}

impl<B> tower_service::Service<Request<B>> for HttpServer
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: fmt::Display,
{
    type Response = Response<Full<Bytes>>;
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
