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
use clapham_cli::{Backend, Cli, McpBackend};
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
    .refusal("list_access_denied", clapham::Category::Refused)
    .streams(true);
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
    let backend = McpBackend::http(format!("{endpoint}/mcp"), "assistant")
        .await
        .unwrap();
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
        let refusal = McpBackend::http(format!("{endpoint}/mcp"), token)
            .await
            .unwrap_err();
        assert_eq!(
            (refusal.code.as_str(), refusal.category),
            ("mcp_unauthenticated", clapham::Category::Refused)
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
    let challenge = http
        .post(format!("{endpoint}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .json(&super_request("lists.export"))
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
    let mut allowed = super_request("lists.delete");
    allowed["params"]["arguments"] = json!({"id":"tasks"});
    let allowed: Value = http
        .post(format!("{endpoint}/mcp"))
        .bearer_auth("operator")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "lists.delete")
        .header("mcp-protocol-version", "2026-07-28")
        .header("accept", "application/json, text/event-stream")
        .json(&allowed)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(allowed["result"]["isError"], false, "{allowed}");
    stop.cancel();
    task.await.unwrap();
    auth_stop.cancel();
    auth_task.await.unwrap();
}

fn super_request(name: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{"id":"tasks"},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})
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

#[tokio::test]
async fn oversized_terminal_outcomes_report_the_size_error_without_rewriting_domain_refusals() {
    let limit = 8192;
    let overhead = serde_json::to_vec(&clapham::Outcome::new(json!("")))
        .unwrap()
        .len();
    let payload = "x".repeat(limit - overhead);
    assert_eq!(
        serde_json::to_vec(&clapham::Outcome::new(json!(&payload)))
            .unwrap()
            .len(),
        limit
    );
    let output = TypedOperation::<ListInput, String>::new(
        "lists.output",
        "Return a bounded result.",
        Effect::Query,
    )
    .unwrap();
    let refusal = TypedOperation::<ListInput, String>::new(
        "lists.refuse",
        "Return a large refusal.",
        Effect::Query,
    )
    .unwrap()
    .refusal("original_code", clapham::Category::Conflict);
    let provider = Contribution::new("lists")
        .typed_operation(output.clone(), move |_: Call, _: ListInput| {
            let payload = payload.clone();
            async move { Ok(payload) }
        })
        .typed_operation(refusal.clone(), move |_: Call, _: ListInput| async move {
            Err(Refusal::new(
                "original_code",
                clapham::Category::Conflict,
                "x".repeat(limit),
            ))
        });
    let bounds = Bounds {
        message_bytes: limit,
        schema_bytes: limit,
        ..Bounds::default()
    };
    let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .provider("lists", provider)
        .expose(Exposure::typed("lists", &output))
        .expose(Exposure::typed("lists", &refusal))
        .compose(bounds.clone())
        .unwrap();
    let server = HttpServer::new(set, |credential: &clapham_mcp_server::Credential| {
        (credential.authorization() == Some("Bearer controlled")).then(|| {
            OAuthContext::from_verified(
                "https://identity.example.com",
                "https://lists.example.com",
                "alice",
                std::iter::empty::<String>(),
            )
        })
    })
    .unwrap();
    let (endpoint, stop, task) = serve(server.into_router()).await;
    let backend = McpBackend::http(format!("{endpoint}/mcp"), "controlled")
        .await
        .unwrap();
    // The size refusal replaces an oversized result; an oversized domain refusal fails the
    // reply rather than changing its code.
    for (name, code) in [
        ("lists.output", "result_too_large"),
        ("lists.refuse", "mcp_transport"),
    ] {
        let error = backend
            .call(
                clapham_cli::Invocation::new(name, json!({"id":"tasks"})),
                Default::default(),
                Default::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.evidence, clapham::Evidence::Uncertain);
    }
    stop.cancel();
    task.await.unwrap();
}
