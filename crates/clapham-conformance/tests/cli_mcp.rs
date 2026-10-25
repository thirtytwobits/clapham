//! The server-backed CLI against its daemon's MCP server over Streamable HTTP and stdio.

use std::sync::{Arc, Mutex};

use clapham::{
    Bounds, Call, Cancellation, Category, ComposedSet, Configuration, Contribution, Evidence,
    Exposure, OAuthContext, Operation, Outcome, RecordSink,
};
use clapham_cli::{Backend, Invocation, McpBackend};
use clapham_mcp_server::{Credential, HttpServer, Server};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

const ISSUER: &str = "https://identity.example.com";
const AUDIENCE: &str = "https://lists.example.com";

fn operator() -> OAuthContext {
    OAuthContext::from_verified(ISSUER, AUDIENCE, "operator", std::iter::empty::<String>())
}

fn descriptor(name: &str, preview: bool) -> Operation {
    serde_json::from_value(json!({
        "name":name,"description":"Return a scripted outcome.",
        "input":{"type":"object"},"effect":"query","locality":"workspace","preview":preview,
        "idempotent":true,"open_world":false,
    }))
    .unwrap()
}

/// Fires when the held handler's future is dropped.
struct Dropped(Option<oneshot::Sender<()>>);
impl Drop for Dropped {
    fn drop(&mut self) {
        if let Some(dropped) = self.0.take() {
            let _ = dropped.send(());
        }
    }
}

struct Hold {
    entered: Arc<Notify>,
    dropped: oneshot::Receiver<()>,
}

/// Operations whose outcomes MCP alone cannot distinguish, a dry run and a held call.
fn set() -> (ComposedSet, Hold) {
    let entered = Arc::new(Notify::new());
    let (dropped, on_drop) = oneshot::channel();
    let dropped = Mutex::new(Some(dropped));
    let signal = entered.clone();
    let mut absent = Outcome::new(Value::Null);
    absent.result = None;
    let mut empty = Outcome::new(json!(1));
    empty.content = Some(Vec::new());
    let provider = Contribution::new("items")
        .operation(descriptor("items.null", false), |_: Call| async {
            Ok(Outcome::new(Value::Null))
        })
        .operation(descriptor("items.absent", false), move |_: Call| {
            let absent = absent.clone();
            async move { Ok(absent) }
        })
        .operation(descriptor("items.empty", false), move |_: Call| {
            let empty = empty.clone();
            async move { Ok(empty) }
        })
        .operation(descriptor("items.preview", true), |call: Call| async move {
            Ok(Outcome::new(json!(call.preview)))
        })
        .operation(descriptor("items.hold", false), move |_: Call| {
            let guard = Dropped(dropped.lock().unwrap().take());
            let signal = signal.clone();
            async move {
                let _guard = guard;
                signal.notify_one();
                std::future::pending::<Result<Outcome, clapham::Refusal>>().await
            }
        });
    let mut configuration = Configuration::new(ISSUER, AUDIENCE).provider("items", provider);
    for name in ["null", "absent", "empty", "preview", "hold"] {
        configuration = configuration.expose(Exposure::new("items", format!("items.{name}")));
    }
    let hold = Hold {
        entered,
        dropped: on_drop,
    };
    (configuration.compose(Bounds::default()).unwrap(), hold)
}

fn http_server(set: ComposedSet) -> HttpServer {
    HttpServer::new(set, |credential: &Credential| {
        (credential.authorization() == Some("Bearer operator")).then(operator)
    })
    .unwrap()
}

/// A daemon serving `set` over Streamable HTTP, and a backend holding the operator's token.
async fn over_http(set: ComposedSet) -> (McpBackend, tokio::task::JoinHandle<()>) {
    serve_http(http_server(set)).await
}

async fn serve_http(server: HttpServer) -> (McpBackend, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, server.into_router()).await.unwrap();
    });
    (McpBackend::http(endpoint, "operator").await.unwrap(), task)
}

/// A daemon serving `set` over a private stdio connection bound to the operator.
async fn over_stdio(set: ComposedSet) -> (McpBackend, tokio::task::JoinHandle<()>) {
    serve_stdio(Server::new(set, operator()).unwrap()).await
}

async fn serve_stdio(server: Server) -> (McpBackend, tokio::task::JoinHandle<()>) {
    let (client, daemon) = tokio::io::duplex(64 * 1024);
    let (read, write) = tokio::io::split(daemon);
    let task = tokio::spawn(async move {
        let _ = server.serve_io(read, write).await.unwrap().waiting().await;
    });
    let (read, write) = tokio::io::split(client);
    (McpBackend::stdio(read, write).await.unwrap(), task)
}

async fn call(
    backend: &McpBackend,
    name: &str,
    preview: bool,
) -> Result<Outcome, clapham::Refusal> {
    let mut invocation = Invocation::new(name, json!({}));
    invocation.preview = preview;
    backend
        .call(invocation, RecordSink::default(), Cancellation::new())
        .await
}

async fn keeps_outcomes(backend: McpBackend, set: &ComposedSet) {
    for name in ["items.null", "items.absent", "items.empty"] {
        let expected = set
            .call(
                &set.all(),
                operator(),
                name,
                json!({}),
                RecordSink::default(),
                Cancellation::new(),
                false,
            )
            .await
            .unwrap();
        let actual = call(&backend, name, false).await.unwrap();
        assert_eq!(
            (
                actual.result,
                actual.content,
                actual.is_error,
                actual.evidence
            ),
            (
                expected.result,
                expected.content,
                expected.is_error,
                expected.evidence
            ),
            "{name}"
        );
    }
}

#[tokio::test]
async fn the_cli_receives_the_outcome_the_operation_returned_over_http_and_stdio() {
    let (set, _) = set();
    let (backend, task) = over_http(set.clone()).await;
    keeps_outcomes(backend, &set).await;
    task.abort();
    let (backend, task) = over_stdio(set.clone()).await;
    keeps_outcomes(backend, &set).await;
    drop(task);
}

async fn discovers_the_selection(backend: McpBackend) {
    let names: Vec<_> = backend
        .catalogue()
        .await
        .unwrap()
        .into_iter()
        .map(|operation| operation.name.to_string())
        .collect();
    assert_eq!(names, ["items.null"]);
    let cli = clapham_cli::Cli::connect("items", backend, Bounds::default())
        .await
        .unwrap();
    let outcome = cli
        .call(
            Invocation::new("items.null", json!({})),
            RecordSink::default(),
            Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.result, Some(Value::Null));
}

#[tokio::test]
async fn the_cli_discovers_a_selection_that_does_not_expose_describe() {
    let (set, _) = set();
    let server = http_server(set.clone()).select(["items.null"]).unwrap();
    let (backend, task) = serve_http(server).await;
    discovers_the_selection(backend).await;
    task.abort();
    let server = Server::new(set, operator())
        .unwrap()
        .select(["items.null"])
        .unwrap();
    let (backend, _task) = serve_stdio(server).await;
    discovers_the_selection(backend).await;
}

#[tokio::test]
async fn a_cli_dry_run_reaches_the_daemon_as_preview_metadata() {
    let (set, _) = set();
    let (backend, task) = over_http(set).await;
    for preview in [false, true] {
        let outcome = call(&backend, "items.preview", preview).await.unwrap();
        assert_eq!(outcome.result, Some(json!(preview)));
    }
    task.abort();
}

async fn cancels(backend: McpBackend, hold: Hold) {
    let cancel = Cancellation::new();
    // The connection stays open, so the daemon learns of the cancellation from the call.
    let calling = {
        let (backend, cancel) = (backend.clone(), cancel.clone());
        tokio::spawn(async move {
            backend
                .call(
                    Invocation::new("items.hold", json!({})),
                    RecordSink::default(),
                    cancel,
                )
                .await
        })
    };
    hold.entered.notified().await;
    cancel.cancel();
    let refusal = calling.await.unwrap().unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category, refusal.evidence),
        ("cancelled", Category::Interrupted, Evidence::Uncertain)
    );
    // Cancelling closes the call's response stream, or over stdio sends
    // `notifications/cancelled`, and the daemon drops the handler.
    hold.dropped.await.unwrap();
    drop(backend);
}

#[tokio::test]
async fn cancelling_a_cli_call_drops_the_daemons_handler_over_http() {
    let (set, hold) = set();
    let (backend, task) = over_http(set).await;
    cancels(backend, hold).await;
    task.abort();
}

#[tokio::test]
async fn cancelling_a_cli_call_drops_the_daemons_handler_over_stdio() {
    let (set, hold) = set();
    let (backend, _task) = over_stdio(set).await;
    cancels(backend, hold).await;
}
