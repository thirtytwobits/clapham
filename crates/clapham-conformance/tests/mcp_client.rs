//! Remote contributions preserve local results and refusal codes through both transports.

use clapham::{
    Bounds, Cancellation, ComposeOptions, ComposedSet, Evidence, ExecutionMode, Principal,
    PrincipalKind, RecordSink, compose, compose_with_options,
};
use clapham_conformance::ListsHandle;
use clapham_mcp_client::{McpConfig, McpConnection, McpHttpOptions, McpSelection};
use clapham_mcp_server::{Credential, HttpServer, Server};
use serde_json::{Value, json};

fn principal() -> Principal {
    Principal::new("host", PrincipalKind::Human)
}
fn fixture() -> (ListsHandle, ComposedSet) {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    let set = compose(handle.contributions(), Bounds::default()).unwrap();
    (handle, set)
}
async fn invoke(set: &ComposedSet, name: &str, input: Value) -> clapham::CallResult {
    set.call(
        &set.all(),
        principal(),
        name,
        input,
        RecordSink::default(),
        Cancellation::new(),
        false,
    )
    .await
}
async fn exercise(connection: &McpConnection, local: &ComposedSet) -> ComposedSet {
    let catalogue = connection.discover().await.unwrap();
    assert!(
        catalogue.diagnostics().is_empty(),
        "{:?}",
        catalogue.diagnostics()
    );
    let binding = catalogue
        .bind(vec![
            McpSelection::new("lists.get"),
            McpSelection::new("lists.compact"),
        ])
        .unwrap();
    assert_eq!(binding.operations()[0].name.as_str(), "remote.lists.get");
    assert_eq!(binding.operations()[0].effect, clapham::Effect::Query);
    assert_eq!(binding.operations()[0].locality, clapham::Locality::Daemon);
    // The server's administrative marker must not widen access through a binding.
    assert_eq!(binding.operations()[1].effect, clapham::Effect::Destructive);
    assert!(
        !binding.operations()[1]
            .principals
            .contains(&PrincipalKind::Agent)
    );
    let remote = compose_with_options(
        [binding.contribution()],
        Bounds::default(),
        ComposeOptions {
            execution: ExecutionMode::Daemon,
            ..Default::default()
        },
    )
    .unwrap();
    let denied = remote
        .call(
            &remote.all(),
            Principal::new("agent", PrincipalKind::Agent),
            "remote.lists.compact",
            json!({}),
            RecordSink::default(),
            Cancellation::new(),
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, "not_admitted");
    assert_eq!(denied.evidence, Evidence::NotDispatched);
    let local_result = invoke(local, "lists.get", json!({"id":"tasks"}))
        .await
        .unwrap();
    let remote_result = invoke(&remote, "remote.lists.get", json!({"id":"tasks"}))
        .await
        .unwrap();
    assert_eq!(remote_result.result, local_result.result);
    assert_eq!(remote_result.evidence, Evidence::Completed);
    assert!(!remote_result.is_error);
    let local_error = invoke(local, "lists.get", json!({"id":"absent"}))
        .await
        .unwrap_err();
    let remote_error = invoke(&remote, "remote.lists.get", json!({"id":"absent"}))
        .await
        .unwrap();
    assert!(remote_error.is_error);
    assert_eq!(
        remote_error.result,
        Some(serde_json::to_value(&local_error).unwrap())
    );
    assert_eq!(remote_error.result.unwrap()["code"], "list_not_found");
    remote
}

#[tokio::test]
async fn stdio_binding_calls_the_same_lists_contribution() {
    let (handle, local) = fixture();
    let server = Server::new(local.clone(), principal()).unwrap();
    let (client_io, server_io) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(server_io);
    let task = tokio::spawn(async move {
        server
            .serve_io(read, write)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let (read, write) = tokio::io::split(client_io);
    let connection = McpConnection::stdio(
        read,
        write,
        Default::default(),
        McpConfig::new("remote", "scope-1"),
    )
    .await
    .unwrap();
    let remote = exercise(&connection, &local).await;

    let mut held = handle.hold("lists.get").unwrap();
    let cancel = Cancellation::new();
    let call_cancel = cancel.clone();
    let call = tokio::spawn(async move {
        remote
            .call(
                &remote.all(),
                principal(),
                "remote.lists.get",
                json!({"id":"tasks"}),
                RecordSink::default(),
                call_cancel,
                false,
            )
            .await
    });
    assert!(held.entered().await);
    cancel.cancel();
    let refusal = call.await.unwrap().unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    connection.close();
    task.await.unwrap();
    assert!(!held.release());
}

#[tokio::test]
async fn http_binding_authenticates_and_calls_the_same_lists_contribution() {
    let (_, local) = fixture();
    let server = HttpServer::new(local.clone(), |credential: &Credential| {
        (credential.authorization() == Some("Bearer authority")).then(principal)
    })
    .unwrap();
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::any(move |request| {
            let server = server.clone();
            async move { server.handle(request).await }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut options = McpHttpOptions::new(format!("http://{}/mcp", listener.local_addr().unwrap()));
    options.bearer_token = Some("authority".into());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let connection = McpConnection::http(options, McpConfig::new("remote", "scope-1"))
        .await
        .unwrap();
    exercise(&connection, &local).await;
    connection.close();
    task.abort();
}
