//! Stateless HTTP authentication, dispatch, cancellation and bounds.

#[path = "support/http.rs"]
mod support;

use std::{
    convert::Infallible,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use clapham::{
    Admission, Bounds, Call, ComposeOptions, Contribution, OAuthContext, Operation, Outcome,
};
use clapham_conformance::{ListsGate, ListsHandle};
use clapham_mcp_server::{Credential, HttpServer};
use futures_channel::oneshot;
use http::{Request, StatusCode};
use http_body::{Body, Frame};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower_service::Service;

fn fixture() -> (ListsHandle, HttpServer) {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    let set = support::compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    (
        handle,
        HttpServer::new(set, support::authenticate)
            .unwrap()
            .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
            .unwrap(),
    )
}

#[tokio::test]
async fn each_request_authenticates_independently_without_a_session() {
    let (_, mut server) = fixture();
    for (credential, administrative) in [
        ("Bearer human", true),
        ("Bearer agent", false),
        ("Bearer human", true),
    ] {
        // Reusing the same JSON-RPC ID across independent HTTP requests is valid.
        let response = server
            .call(support::request("tools/list", json!({}), credential))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("mcp-session-id"));
        let reply: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        assert_eq!(
            tools.iter().any(|t| t["name"] == "lists.compact"),
            administrative
        );
        assert_eq!(reply["result"]["cacheScope"], "private");
    }
    let denied = server.handle(support::request("tools/call",json!({"name":"lists.compact","arguments":{},"_meta":{"subject":"alice","client_id":"operator","scopes":["lists:admin"]}}),"Bearer agent")).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(
        denied.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("lists:admin")
    );
    let denied: clapham::Refusal =
        serde_json::from_slice(&denied.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(denied.code, "insufficient_scope");
    assert_eq!(denied.evidence, clapham::Evidence::NotDispatched);
    let refused = server
        .handle(support::request("tools/list", json!({}), "Bearer unknown"))
        .await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
}

struct UnreadBody;
impl Body for UnreadBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        panic!("authentication must precede body consumption")
    }
}

#[tokio::test]
async fn unmapped_credentials_are_refused_before_body_or_gate_or_handler() {
    let handle = ListsHandle::new();
    let gate_calls = Arc::new(AtomicUsize::new(0));
    let observed = gate_calls.clone();
    let set = support::compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                observed.fetch_add(1, Ordering::SeqCst);
                Admission::Allow
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate).unwrap();
    let response = server
        .handle(
            Request::post("/mcp")
                .header("host", "localhost")
                .header("authorization", "Bearer unknown")
                .body(UnreadBody)
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let refusal: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(refusal["code"], "not_authenticated");
    assert_eq!(refusal["evidence"], "not_dispatched");
    assert_eq!(gate_calls.load(Ordering::SeqCst), 0);
    assert!(handle.snapshot().is_empty());
    let missing = server
        .handle(
            Request::post("/mcp")
                .header("host", "localhost")
                .body(UnreadBody)
                .unwrap(),
        )
        .await;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn selected_calls_preview_refusals_and_resources_share_the_http_mapping() {
    let (handle, server) = fixture();
    let preview = support::result(&server, "tools/call", json!({"name":"lists.create","arguments":{"list":{"id":"preview","name":"Preview"}},"_meta":{"io.github.thirtytwobits.clapham/preview":true}}), "Bearer human").await;
    assert_eq!(preview["isError"], false);
    assert_eq!(handle.snapshot().len(), 1);
    let selected = server.select(["lists.get"]).unwrap();
    let listed = support::result(&selected, "tools/list", json!({}), "Bearer human").await;
    assert_eq!(listed["tools"].as_array().unwrap().len(), 1);
    let got = support::result(
        &selected,
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"tasks"}}),
        "Bearer human",
    )
    .await;
    let resource = support::result(
        &selected,
        "resources/read",
        json!({"uri":"lists://tasks"}),
        "Bearer human",
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(resource["contents"][0]["text"].as_str().unwrap()).unwrap(),
        got["structuredContent"]
    );
    let missing = support::result(
        &selected,
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"missing"}}),
        "Bearer human",
    )
    .await;
    assert_eq!(missing["structuredContent"]["code"], "list_not_found");
    assert_eq!(missing["structuredContent"]["category"], "not_found");
}

#[tokio::test]
async fn http_protocol_and_origin_checks_are_enforced_without_sessions() {
    let (_, server) = fixture();
    let mut request = support::request("tools/list", json!({}), "Bearer human");
    request
        .headers_mut()
        .insert("mcp-method", "tools/call".parse().unwrap());
    assert_eq!(
        server.handle(request).await.status(),
        StatusCode::BAD_REQUEST
    );
    let mut request = support::request("tools/list", json!({}), "Bearer human");
    request
        .headers_mut()
        .insert("origin", "https://unlisted.invalid".parse().unwrap());
    assert_eq!(server.handle(request).await.status(), StatusCode::FORBIDDEN);
    let mut request = support::request("tools/list", json!({}), "Bearer human");
    *request.method_mut() = http::Method::GET;
    assert_eq!(
        server.handle(request).await.status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let mut request = support::request("tools/list", json!({}), "Bearer human");
    request
        .headers_mut()
        .append("authorization", "Bearer agent".parse().unwrap());
    assert_eq!(
        server.handle(request).await.status(),
        StatusCode::BAD_REQUEST
    );
}

fn probe() -> Operation {
    serde_json::from_value(json!({"name":"probe.large","description":"Read a large result.","input":{"type":"object"},"effect":"query","locality":"workspace","idempotent":true,"open_world":false})).unwrap()
}

#[tokio::test]
async fn http_request_and_reply_bytes_are_bounded() {
    let set = support::compose(
        [
            Contribution::new("probe").operation(probe(), |_: Call| async {
                Ok(Outcome::new(json!("x".repeat(16384))))
            }),
        ],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate)
        .unwrap()
        .resources([("probe.large", "probe://large")])
        .unwrap();
    let response = server
        .handle(support::request(
            "tools/call",
            json!({"name":"probe.large","arguments":{"padding":"x".repeat(16384)}}),
            "Bearer human",
        ))
        .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let result = support::result(
        &server,
        "tools/call",
        json!({"name":"probe.large"}),
        "Bearer human",
    )
    .await;
    assert_eq!(result["structuredContent"]["code"], "result_too_large");
    let result = support::result(
        &server,
        "resources/read",
        json!({"uri":"probe://large"}),
        "Bearer human",
    )
    .await;
    let refusal: Value =
        serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refusal["code"], "result_too_large");
}

struct Dropped(Option<oneshot::Sender<()>>);
impl Drop for Dropped {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

#[tokio::test]
async fn bounded_queue_and_disconnect_cancel_without_timers() {
    use std::sync::Mutex;
    let (entered, enter) = oneshot::channel();
    let (dropped, drop_seen) = oneshot::channel();
    let signals = Mutex::new(Some((entered, dropped)));
    let set = support::compose(
        [
            Contribution::new("probe").operation(probe(), move |_: Call| {
                let (entered, dropped) = signals.lock().unwrap().take().unwrap();
                async move {
                    let _drop = Dropped(Some(dropped));
                    let _ = entered.send(());
                    std::future::pending().await
                }
            }),
        ],
        Bounds {
            in_flight: 1,
            waiting: 0,
            ..Bounds::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate).unwrap();
    let mut held = Box::pin(server.handle(support::request(
        "tools/call",
        json!({"name":"probe.large"}),
        "Bearer human",
    )));
    tokio::select! { _ = &mut held => panic!("handler must remain held"), signal = enter => signal.unwrap() }
    let full = server
        .handle(support::request("tools/list", json!({}), "Bearer human"))
        .await;
    assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(held);
    drop_seen.await.unwrap();
    let next = support::result(
        &server,
        "tools/call",
        json!({"name":"clapham.version"}),
        "Bearer human",
    )
    .await;
    assert_eq!(next["isError"], false);
}

#[test]
fn credentials_do_not_expose_authorization_in_debug_output() {
    let credential = Credential::new("Bearer secret");
    assert!(!format!("{credential:?}").contains("secret"));
    assert_eq!(credential.authorization(), Some("Bearer secret"));
}

#[tokio::test]
async fn concurrent_requests_keep_their_own_authenticated_auths() {
    use std::sync::Mutex;
    let (entered, enter) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let signals = Mutex::new(Some((entered, released)));
    let mut descriptor = probe();
    descriptor.name = "probe.identity".parse().unwrap();
    let set = support::compose(
        [
            Contribution::new("probe").operation(descriptor, move |call: Call| {
                let signals = if call.auth.has_scope("lists:admin") {
                    signals.lock().unwrap().take()
                } else {
                    None
                };
                async move {
                    if let Some((entered, released)) = signals {
                        let _ = entered.send(());
                        released.await.unwrap();
                    }
                    Ok(Outcome::new(serde_json::to_value(call.auth).unwrap()))
                }
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate).unwrap();
    let mut human = Box::pin(server.handle(support::request(
        "tools/call",
        json!({"name":"probe.identity"}),
        "Bearer human",
    )));
    tokio::select! { _ = &mut human => panic!("the human call must remain held"), signal = enter => signal.unwrap() }
    let agent = support::result(
        &server,
        "tools/call",
        json!({"name":"probe.identity"}),
        "Bearer agent",
    )
    .await;
    assert_eq!(
        agent["structuredContent"]["scopes"],
        json!(["lists:read", "lists:write"])
    );
    release.send(()).unwrap();
    let response = human.await;
    let reply: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        reply["result"]["structuredContent"]["scopes"],
        json!(["items:tick", "lists:admin", "lists:read", "lists:write"])
    );
}

#[tokio::test]
async fn oversized_declared_refusal_returns_http_failure_without_replacing_its_code() {
    let mut descriptor = probe();
    descriptor
        .refusals
        .insert("original_code".into(), clapham::Category::Conflict);
    let set = support::compose(
        [
            Contribution::new("probe").operation(descriptor, |_: Call| async {
                Err(clapham::Refusal::new(
                    "original_code",
                    clapham::Category::Conflict,
                    "x".repeat(16384),
                ))
            }),
        ],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate)
        .unwrap()
        .resources([("probe.large", "probe://large")])
        .unwrap();
    for (method, params) in [
        ("tools/call", json!({"name":"probe.large"})),
        ("resources/read", json!({"uri":"probe://large"})),
    ] {
        let response = server
            .handle(support::request(method, params, "Bearer human"))
            .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.len() <= 8192);
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(reply["error"]["code"], -32603);
        assert!(reply.get("result").is_none());
    }
}

#[tokio::test]
async fn rejected_host_and_origin_never_reach_authentication_or_the_body() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let server = HttpServer::new(
        support::compose([], Bounds::default()).unwrap(),
        move |credential: &Credential| {
            observed.fetch_add(1, Ordering::SeqCst);
            support::authenticate(credential)
        },
    )
    .unwrap();
    for credential in ["Bearer human", "Bearer unknown"] {
        for (host, origin) in [
            ("unlisted.invalid", None),
            ("localhost", Some("https://unlisted.invalid")),
        ] {
            let mut request = Request::post("/mcp")
                .header("host", host)
                .header("authorization", credential);
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = server.handle(request.body(UnreadBody).unwrap()).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn configured_host_and_origin_reuse_rmcp_authority_matching() {
    let server = HttpServer::new(
        support::compose([], Bounds::default()).unwrap(),
        support::authenticate,
    )
    .unwrap()
    .with_allowed_hosts(["[::1]:8443"])
    .with_allowed_origins(["https://client.invalid:443"]);
    let mut request = support::request("tools/list", json!({}), "Bearer human");
    request
        .headers_mut()
        .insert("host", "[::1]:8443".parse().unwrap());
    request
        .headers_mut()
        .insert("origin", "https://client.invalid".parse().unwrap());
    assert_eq!(server.handle(request).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn authentication_challenge_advertises_the_resource_and_accepts_host_overrides() {
    let server = HttpServer::new(
        support::compose([], Bounds::default()).unwrap(),
        support::authenticate,
    )
    .unwrap();
    let response = server
        .handle(support::request("tools/list", json!({}), "Bearer unknown"))
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers()["www-authenticate"],
        "Bearer resource_metadata=\"https://lists.example.com/.well-known/oauth-protected-resource\""
    );
    for challenge in [
        "Basic realm=\"lists\"",
        "Bearer resource_metadata=\"https://host.invalid/.well-known/oauth-protected-resource\"",
    ] {
        let configured = server
            .clone()
            .with_authentication_challenge(challenge.parse().unwrap());
        let response = configured
            .handle(support::request("tools/list", json!({}), "Bearer unknown"))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.headers()["www-authenticate"], challenge);
    }
}

#[tokio::test]
async fn empty_authorization_is_distinct_from_missing_authorization() {
    use std::sync::Mutex;
    let values = Arc::new(Mutex::new(Vec::new()));
    let seen = values.clone();
    let server = HttpServer::new(
        support::compose([], Bounds::default()).unwrap(),
        move |credential: &Credential| {
            seen.lock()
                .unwrap()
                .push(credential.authorization().map(str::to_owned));
            None
        },
    )
    .unwrap();
    let missing = Request::post("/mcp")
        .header("host", "localhost")
        .body(UnreadBody)
        .unwrap();
    assert_eq!(
        server.handle(missing).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let empty = Request::post("/mcp")
        .header("host", "localhost")
        .header("authorization", "")
        .body(UnreadBody)
        .unwrap();
    assert_eq!(
        server.handle(empty).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(*values.lock().unwrap(), vec![None, Some(String::new())]);
}

#[tokio::test]
async fn a_long_request_id_cannot_force_an_oversized_replacement_refusal() {
    let set = support::compose(
        [
            Contribution::new("probe").operation(probe(), |_: Call| async {
                Ok(Outcome::new(json!("x".repeat(8192))))
            }),
        ],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate).unwrap();
    let request = support::request("tools/call", json!({"name":"probe.large"}), "Bearer human");
    let (parts, body) = request.into_parts();
    let mut value: Value =
        serde_json::from_slice(&body.collect().await.unwrap().to_bytes()).unwrap();
    value["id"] = json!("x".repeat(7600));
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() < 8192);
    let request = Request::from_parts(parts, http_body_util::Full::new(Bytes::from(bytes)));
    let response = server.handle(request).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .len()
            <= 8192
    );
}

#[test]
fn http_requires_an_explicit_oauth_resource_binding() {
    let unbound = clapham::compose([], Bounds::default()).unwrap();
    let refusal = HttpServer::new(unbound, |_: &Credential| -> Option<OAuthContext> {
        panic!("configuration must fail before authentication")
    })
    .unwrap_err();
    assert_eq!(refusal.code, "mcp_configuration");
    assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
}

#[tokio::test]
async fn mismatched_verified_authority_is_rejected() {
    for (issuer, audience) in [
        ("https://other.example.com", "https://lists.example.com"),
        ("https://identity.example.com", "https://other.example.com"),
    ] {
        let server = HttpServer::new(
            support::compose([], Bounds::default()).unwrap(),
            move |_: &Credential| {
                Some(OAuthContext::from_verified(
                    issuer,
                    audience,
                    "alice",
                    std::iter::empty::<String>(),
                ))
            },
        )
        .unwrap();
        let mcp = server
            .handle(support::request(
                "tools/list",
                json!({}),
                "Bearer controlled",
            ))
            .await;
        assert_eq!(mcp.status(), StatusCode::UNAUTHORIZED);
    }
}
