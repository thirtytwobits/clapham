//! Bounded HTTP JSON/SSE discovery, dispatch and refusal contracts.
#![cfg(feature = "http")]
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use clapham::{Call, Evidence};
use clapham_mcp_client::*;
mod support;
use support::*;

use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
struct Server {
    mode: AtomicUsize,
    calls: Counter,
}
async fn handle(State(server): State<Arc<Server>>, Json(request): Json<Value>) -> Response {
    let mut result = match request["method"].as_str().unwrap() {
        "server/discover" => {
            json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
        }
        "tools/list" => {
            json!({"tools":[{"name":"write","description":"Write","inputSchema":{"type":"object"},"_meta":{"raw":"provenance"},"extension":{"retained":true}},{"name":"invalid","description":"Invalid record","inputSchema":false}]})
        }
        "tools/call" => {
            server.calls.fetch_add(1, Ordering::SeqCst);
            match server.mode.load(Ordering::SeqCst) {
                1 => {
                    std::future::pending::<()>().await;
                }
                2 => {
                    return (
                        [("content-type", "application/json")],
                        "x".repeat(128 * 1024),
                    )
                        .into_response();
                }
                3 => {
                    return (
                        [("content-type", "text/event-stream")],
                        format!("data: {}", "x".repeat(128 * 1024)),
                    )
                        .into_response();
                }
                4 => return StatusCode::NOT_FOUND.into_response(),
                _ => (),
            }
            json!({"content":[{"type":"text","text":"done"}],"structuredContent":null})
        }
        _ => return StatusCode::ACCEPTED.into_response(),
    };
    if matches!(server.mode.load(Ordering::SeqCst), 9 | 10) && request["method"] == "tools/call" {
        result["tools"] = json!([]);
    }
    let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
    let mode = server.mode.load(Ordering::SeqCst);
    if matches!(mode, 5 | 8 | 10) {
        (
            [(
                "content-type",
                if mode == 8 {
                    "Text/Event-Stream ; charset=utf-8"
                } else {
                    "text/event-stream"
                },
            )],
            format!("data: {response}\n\n"),
        )
            .into_response()
    } else {
        let mut response = Json(response).into_response();
        if mode == 7 {
            response.headers_mut().insert(
                "content-type",
                "Application/JSON ; charset=utf-8".parse().unwrap(),
            );
        }
        response
    }
}
async fn setup(mode: usize) -> (McpConnection, Arc<Server>, tokio::task::JoinHandle<()>) {
    let server = Arc::new(Server {
        mode: AtomicUsize::new(mode),
        calls: Counter::new(0),
    });
    let app = Router::new()
        .route("/mcp", post(handle))
        .with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = McpConfig::new("remote", "scope-1");
    config.operation_timeout = Duration::from_secs(3600);
    config.bounds.message_bytes = 64 * 1024;
    config.bounds.schema_bytes = 32 * 1024;
    let connection = McpConnection::http(McpHttpOptions::new(endpoint), config)
        .await
        .unwrap();
    (connection, server, task)
}
fn call() -> Call {
    support::call("remote.write", json!({}))
}
#[tokio::test]
async fn json_and_sse_discovery_and_invocation_preserve_results() {
    for mode in [0, 5, 7, 8, 9, 10] {
        let (connection, server, task) = setup(mode).await;
        let catalogue = connection.discover().await.unwrap();
        assert_eq!(
            catalogue.provenance("write").unwrap()["_meta"]["raw"],
            "provenance"
        );
        assert_eq!(
            catalogue.provenance("write").unwrap()["extension"]["retained"],
            true
        );
        assert_eq!(catalogue.diagnostics().len(), 1);
        assert_eq!(catalogue.diagnostics()[0].name.as_deref(), Some("invalid"));
        let binding = catalogue.bind(vec![McpSelection::new("write")]).unwrap();
        let result = binding.executor().call(call()).await.unwrap();
        assert_eq!(result.result.as_ref(), Some(&Value::Null));
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        connection.close();
        task.abort();
    }
}
#[tokio::test]
async fn lost_response_limits_and_session_expiry_never_replay_posts() {
    for mode in [1, 2, 3, 4] {
        let (connection, server, task) = setup(mode).await;
        let binding = connection
            .discover()
            .await
            .unwrap()
            .bind(vec![McpSelection::new("write")])
            .unwrap();
        let executor = binding.executor();
        let error = if mode == 1 {
            let task = tokio::spawn(async move { executor.call(call()).await });
            server.calls.wait(1).await;
            connection.close();
            task.await.unwrap().unwrap_err()
        } else {
            executor.call(call()).await.unwrap_err()
        };
        assert_eq!(error.evidence, Evidence::Uncertain);
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
        if mode == 2 || mode == 3 {
            assert!(error.message.contains("limit"), "{}", error.message);
        }
        connection.close();
        task.abort();
    }
}
#[test]
fn http_debug_excludes_endpoint_and_credentials() {
    let mut options = McpHttpOptions::new("https://secret.example/?token=secret");
    options.bearer_token = Some("secret".into());
    assert!(!format!("{options:?}").contains("secret"));
}

#[tokio::test]
async fn startup_distinguishes_unavailable_endpoints_from_invalid_responses() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        McpConnection::http(
            McpHttpOptions::new(endpoint),
            McpConfig::new("remote", "scope")
        )
        .await,
        Err(McpError::Transport)
    ));
    for body in [
        "not JSON",
        r#"{"jsonrpc":"2.0","id":0,"result":{"tools":[]}}"#,
    ] {
        let app = Router::new().route(
            "/mcp",
            post(move || async move { ([("content-type", "application/json")], body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async {
            axum::serve(listener, app).await.unwrap();
        });
        assert!(matches!(
            McpConnection::http(
                McpHttpOptions::new(endpoint),
                McpConfig::new("remote", "scope")
            )
            .await,
            Err(McpError::Protocol)
        ));
        task.abort();
    }
}

#[tokio::test]
async fn an_unaccepted_credential_refuses_the_connection_as_unauthenticated() {
    let app = Router::new().route("/mcp", post(|| async { StatusCode::UNAUTHORIZED }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async {
        axum::serve(listener, app).await.unwrap();
    });
    let error = McpConnection::http(
        McpHttpOptions::new(endpoint),
        McpConfig::new("remote", "scope"),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error, McpError::Unauthenticated);
    let refusal = error.refusal(Evidence::NotDispatched);
    assert_eq!(
        (refusal.code.as_str(), refusal.category),
        ("mcp_unauthenticated", clapham::Category::Refused)
    );
    task.abort();
}
