//! OAuth introspection and the server-backed CLI share the operation boundary.

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Json, extract::Form};
use clapham::{
    Bounds, Call, Configuration, Contribution, Effect, Exposure, OAuthContext, OAuthResource,
    RecordSink, Refusal, ResourcePolicy, TypedOperation,
};
use clapham_cli::{Backend, Cli, HttpBackend};
use clapham_mcp_server::{HttpServer, OAuthIntrospector};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListInput {
    id: String,
}

async fn serve(app: axum::Router) -> (String, clapham::Cancellation, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let stop = clapham::Cancellation::new();
    let stopped = stop.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { stopped.cancelled().await })
            .await
            .unwrap();
    });
    (endpoint, stop, task)
}

#[tokio::test]
async fn verified_grants_scopes_resource_policies_and_cli_records_share_one_runner() {
    let resource = OAuthResource {
        issuer: "https://identity.example.com".into(),
        audience: "https://lists.example.com".into(),
    };
    let issuer = resource.issuer.clone();
    let audience = resource.audience.clone();
    let auth = axum::Router::new().route("/introspect",axum::routing::post(move |headers: axum::http::HeaderMap, Form(form): Form<BTreeMap<String,String>>| {
        let issuer = issuer.clone(); let audience = audience.clone();
        async move {
            assert_eq!(headers["authorization"],"Basic Y2xpZW50OnNlY3JldA==");
            let token = form.get("token").map(String::as_str).unwrap();
            if !["assistant","operator","wrong-audience","expired","oversized"].contains(&token) { return Json(json!({"active":false})); }
            Json(json!({"active":true,"iss":issuer,"aud":if token=="wrong-audience" {"other"} else {&audience},"sub":"alice","client_id":token,
                "scope":if token=="operator" {"lists:read lists:delete"} else {"lists:read"},"list_ids":["tasks"],"padding":if token=="oversized" {"x".repeat(65536)} else {String::new()},"exp":if token=="expired" {50} else {1000}}))
        }
    }));
    let (auth_endpoint, auth_stop, auth_task) = serve(auth).await;
    let verifier = OAuthIntrospector::new(
        resource.clone(),
        format!("{auth_endpoint}/introspect"),
        "client",
        "secret",
    )
    .unwrap()
    .with_clock(|| 100);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    let after_first = Arc::new(AtomicUsize::new(0));
    let advanced = after_first.clone();
    let export = TypedOperation::<ListInput, u64>::new(
        "lists.export",
        "Export list records.",
        Effect::Query,
    )
    .unwrap()
    .require_scope("lists:read")
    .policy("lists.can_read")
    .streams(true)
    .refusal("record_delivery_failed", clapham::Category::Unavailable);
    let delete = TypedOperation::<ListInput, u64>::new(
        "lists.delete",
        "Delete a list.",
        Effect::Destructive,
    )
    .unwrap()
    .require_scope("lists:delete");
    let provider = Contribution::new("lists")
        .typed_operation(export, move |call: Call, _: ListInput| {
            let entered = entered.clone();
            let advanced = advanced.clone();
            async move {
                entered.fetch_add(1, Ordering::SeqCst);
                call.records.send(json!({"item":"a"})).await?;
                advanced.fetch_add(1, Ordering::SeqCst);
                call.records.send(json!({"item":"b"})).await?;
                Ok(2)
            }
        })
        .typed_operation(delete, |_: Call, _: ListInput| async { Ok(1) });
    let policy = ResourcePolicy::typed(|auth: OAuthContext, input: ListInput| async move {
        if auth
            .claim("list_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id == &input.id))
        {
            Ok(())
        } else {
            Err(Refusal::new(
                "list_access_denied",
                clapham::Category::Refused,
                "this grant cannot access the list",
            ))
        }
    });
    let set = Configuration::new(&resource.issuer, &resource.audience)
        .provider("lists", provider)
        .register_policy("lists.can_read", policy)
        .expose(Exposure::new("lists", "lists.export"))
        .expose(Exposure::new("lists", "lists.delete"))
        .compose(Bounds::default())
        .unwrap();
    let server = HttpServer::new(set, verifier).unwrap();
    let (endpoint, stop, task) = serve(server.into_router()).await;
    let backend = HttpBackend::new(&endpoint, "assistant", Bounds::default()).unwrap();
    let catalogue = backend.catalogue().await.unwrap();
    assert!(
        catalogue
            .iter()
            .any(|op| op.name.as_str() == "lists.export")
    );
    assert!(
        !catalogue
            .iter()
            .any(|op| op.name.as_str() == "lists.delete")
    );
    for token in ["unknown", "expired", "wrong-audience", "oversized"] {
        let invalid = HttpBackend::new(&endpoint, token, Bounds::default()).unwrap();
        assert_eq!(
            invalid.catalogue().await.unwrap_err().code,
            "not_authenticated"
        );
    }
    let records = Arc::new(Mutex::new(Vec::new()));
    let captured = records.clone();
    let cli = Cli::connect("lists", backend, Bounds::default())
        .await
        .unwrap();
    let (entered, first_record) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let held = Arc::new(Mutex::new(Some((entered, released))));
    let run = cli.run_from(
        ["lists", "lists", "export", "--id", "tasks"],
        std::io::empty(),
        RecordSink::new(move |record| {
            captured.lock().unwrap().push(record);
            let held = held.lock().unwrap().take();
            async move {
                if let Some((entered, released)) = held {
                    entered.send(()).unwrap();
                    released.await.unwrap();
                }
                Ok(())
            }
        }),
        Default::default(),
    );
    tokio::pin!(run);
    tokio::select! { result=&mut run=>panic!("record sink must remain held: {result:?}"),entered=first_record=>entered.unwrap() }
    assert_eq!(after_first.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    let result = run.await.unwrap();
    assert_eq!(result.result, Some(json!(2)));
    assert_eq!(
        *records.lock().unwrap(),
        [json!({"item":"a"}), json!({"item":"b"})]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let refusal = cli
        .run_from(
            ["lists", "lists", "export", "--id", "private"],
            std::io::empty(),
            Default::default(),
            Default::default(),
        )
        .await
        .unwrap_err();
    let clapham_cli::CliError::Refusal(refusal) = refusal else {
        panic!("expected resource-policy refusal")
    };
    assert_eq!(refusal.code, "list_access_denied");
    assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let http = reqwest::Client::new();
    let denied = http
        .post(format!("{endpoint}/call"))
        .bearer_auth("assistant")
        .json(&json!({"name":"lists.delete","input":{"id":"tasks"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    assert!(
        denied.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("lists:delete")
    );
    let allowed = http
        .post(format!("{endpoint}/call"))
        .bearer_auth("operator")
        .json(&json!({"name":"lists.delete","input":{"id":"tasks"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
    assert!(allowed.text().await.unwrap().contains("outcome"));
    let challenge = http
        .get(format!("{endpoint}/operations"))
        .send()
        .await
        .unwrap();
    assert_eq!(challenge.status(), 401);
    assert!(
        challenge.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("resource_metadata=")
    );
    let metadata: Value = http
        .get(format!("{endpoint}/.well-known/oauth-protected-resource"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(metadata["resource"], resource.audience);
    assert_eq!(metadata["authorization_servers"], json!([resource.issuer]));
    let mut mcp = super_request("lists.delete");
    mcp["params"]["_meta"]["subject"] = json!("other-user");
    mcp["params"]["_meta"]["scopes"] = json!(["lists:delete"]);
    let denied = http
        .post(format!("{endpoint}/mcp"))
        .bearer_auth("assistant")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "lists.delete")
        .header("mcp-protocol-version", "2026-07-28")
        .header("accept", "application/json, text/event-stream")
        .json(&mcp)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    assert!(
        denied.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("lists:delete")
    );
    let forged = http.post(format!("{endpoint}/call")).bearer_auth("assistant").json(&json!({"name":"lists.delete","input":{"id":"tasks"},"auth":{"scopes":["lists:delete"]}})).send().await.unwrap();
    assert_eq!(forged.status(), 400);
    stop.cancel();
    task.await.unwrap();
    auth_stop.cancel();
    auth_task.await.unwrap();
}

fn super_request(name: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{"id":"tasks"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})
}

#[tokio::test]
async fn native_http_records_wait_for_client_acknowledgement_and_disconnect_cancels_dispatch() {
    use http_body_util::BodyExt;
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let (dropped, finished) = tokio::sync::oneshot::channel();
    let dropped = Mutex::new(Some(dropped));
    let stage = Arc::new(AtomicUsize::new(0));
    let observed = stage.clone();
    let stream = TypedOperation::<ListInput, u64>::new(
        "lists.export",
        "Export held records.",
        Effect::Query,
    )
    .unwrap()
    .streams(true)
    .refusal("record_delivery_failed", clapham::Category::Unavailable);
    let count = TypedOperation::<ListInput, u64>::new("lists.count", "Count lists.", Effect::Query)
        .unwrap();
    let provider = Contribution::new("lists")
        .typed_operation(stream, move |call: Call, _: ListInput| {
            let dropped = Dropped(Some(dropped.lock().unwrap().take().unwrap()));
            let stage = stage.clone();
            async move {
                let _dropped = dropped;
                stage.store(1, Ordering::SeqCst);
                call.records.send(json!({"item":"a"})).await?;
                stage.store(2, Ordering::SeqCst);
                call.records.send(json!({"item":"b"})).await?;
                std::future::pending::<Result<u64, Refusal>>().await
            }
        })
        .typed_operation(count, |_: Call, _: ListInput| async { Ok(2) });
    let bounds = Bounds {
        in_flight: 1,
        waiting: 0,
        ..Bounds::default()
    };
    let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .provider("lists", provider)
        .expose(Exposure::new("lists", "lists.export"))
        .expose(Exposure::new("lists", "lists.count"))
        .compose(bounds)
        .unwrap();
    let server = HttpServer::new(set, |credential: &clapham_mcp_server::Credential| {
        matches!(
            credential.authorization(),
            Some("Bearer controlled" | "Bearer other-grant")
        )
        .then(|| {
            OAuthContext::from_verified(
                "https://identity.example.com",
                "https://lists.example.com",
                if credential.authorization() == Some("Bearer controlled") {
                    "alice"
                } else {
                    "bob"
                },
                std::iter::empty::<String>(),
            )
        })
    })
    .unwrap();
    let request = |name: &str| {
        http::Request::post("/call")
            .header("host", "localhost")
            .header("authorization", "Bearer controlled")
            .body(http_body_util::Full::new(bytes::Bytes::from(
                json!({"name":name,"input":{"id":"tasks"}}).to_string(),
            )))
            .unwrap()
    };
    let mut body = server
        .handle_operations(request("lists.export"))
        .await
        .into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    let first: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["kind"], "record");
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    let acknowledgement = |ticket: &str, token: &str| {
        http::Request::post(format!("/ack/{ticket}"))
            .header("host", "localhost")
            .header("authorization", format!("Bearer {token}"))
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .unwrap()
    };
    let second = {
        use std::{future::Future, pin::pin, task::Context};
        let mut next = pin!(body.frame());
        let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
        assert!(next.as_mut().poll(&mut cx).is_pending());
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        let ticket = first["ack"].as_str().unwrap();
        let denied = server
            .handle_operations(acknowledgement(ticket, "other-grant"))
            .await;
        assert_eq!(denied.status(), 404);
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        let accepted = server
            .handle_operations(acknowledgement(ticket, "controlled"))
            .await;
        assert_eq!(accepted.status(), 204);
        let duplicate = server
            .handle_operations(acknowledgement(ticket, "controlled"))
            .await;
        assert_eq!(duplicate.status(), 404);
        next.await.unwrap().unwrap().into_data().unwrap()
    };
    let second: Value = serde_json::from_slice(&second).unwrap();
    assert_eq!(second["data"]["item"], "b");
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    drop(body);
    finished.await.unwrap();
    let abandoned = server
        .handle_operations(acknowledgement(
            second["ack"].as_str().unwrap(),
            "controlled",
        ))
        .await;
    assert_eq!(abandoned.status(), 404);
    let response = server.handle_operations(request("lists.count")).await;
    assert_eq!(response.status(), 200);
    let result: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(result["kind"], "outcome");
    assert_eq!(result["data"]["evidence"], "completed");
}

#[tokio::test]
async fn native_http_client_rejects_missing_duplicate_truncated_and_oversized_terminals() {
    let complete = format!(
        "{}\n",
        json!({"kind":"outcome","data":{"result":2,"evidence":"completed"}})
    );
    for (body, valid) in [
        (complete.clone(), true),
        (String::new(), false),
        (format!("{complete}{complete}"), false),
        (complete.trim_end().to_owned(), false),
        (
            format!("{}\n", json!({"kind":"record","data":"x".repeat(8192)})),
            false,
        ),
        (
            format!("{complete}{}\n", json!({"kind":"record","data":1})),
            false,
        ),
    ] {
        let app = axum::Router::new().route(
            "/call",
            axum::routing::post(move || {
                let body = body.clone();
                async move { ([("content-type", "application/x-ndjson")], body) }
            }),
        );
        let (endpoint, stop, task) = serve(app).await;
        let backend = HttpBackend::new(
            endpoint,
            "controlled",
            Bounds {
                message_bytes: 8192,
                schema_bytes: 8192,
                ..Bounds::default()
            },
        )
        .unwrap();
        let result = backend
            .call(
                clapham_cli::Invocation::new("lists.count", json!({})),
                Default::default(),
                Default::default(),
            )
            .await;
        if valid {
            assert_eq!(result.unwrap().result, Some(json!(2)));
        } else {
            let refusal = result.unwrap_err();
            assert_eq!(refusal.code, "connection_lost");
            assert_eq!(refusal.evidence, clapham::Evidence::Uncertain);
        }
        stop.cancel();
        task.await.unwrap();
    }
}

#[tokio::test]
async fn server_backed_cli_keeps_selection_and_stops_before_pre_cancelled_or_zero_deadline_dispatch()
 {
    #[derive(Debug, Clone)]
    struct Counter(Arc<AtomicUsize>);
    impl Backend for Counter {
        fn catalogue(&self) -> clapham_cli::BackendFuture<'_, Vec<clapham::Operation>> {
            Box::pin(async {
                Ok(vec![
                    TypedOperation::<ListInput, u64>::new(
                        "lists.count",
                        "Count lists.",
                        Effect::Query,
                    )
                    .unwrap()
                    .descriptor()
                    .clone(),
                ])
            })
        }
        fn call(
            &self,
            _: clapham_cli::Invocation,
            _: RecordSink,
            _: clapham::Cancellation,
        ) -> clapham_cli::BackendFuture<'_, clapham::Outcome> {
            Box::pin(async {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(clapham::Outcome::new(json!(2)))
            })
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let cli = Cli::connect("lists", Counter(calls.clone()), Bounds::default())
        .await
        .unwrap();
    assert!(
        !cli.command()
            .get_subcommands()
            .any(|command| command.get_name() == "clapham")
    );
    let invocation = || clapham_cli::Invocation::new("lists.count", json!({"id":"tasks"}));
    let cancel = clapham::Cancellation::new();
    cancel.cancel();
    let refused = cli
        .call(invocation(), Default::default(), cancel)
        .await
        .unwrap_err();
    assert_eq!(refused.evidence, clapham::Evidence::NotDispatched);
    let mut zero = invocation();
    zero.timeout = Some(std::time::Duration::ZERO);
    let refused = cli
        .call(zero, Default::default(), Default::default())
        .await
        .unwrap_err();
    assert_eq!(refused.evidence, clapham::Evidence::NotDispatched);
    let mut hidden = invocation();
    hidden.name = "lists.hidden".into();
    assert_eq!(
        cli.call(hidden, Default::default(), Default::default())
            .await
            .unwrap_err()
            .code,
        "unknown_operation"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    cli.call(invocation(), Default::default(), Default::default())
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
