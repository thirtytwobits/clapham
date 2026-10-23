//! Discovery, binding revisions, cancellation and dispatch evidence contracts.
#![cfg(feature = "stdio")]
use clapham::{Call, Evidence, PrincipalKind};
use clapham_mcp_client::*;
mod support;
use support::*;

use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

struct Server {
    tools: Mutex<Vec<Value>>,
    result: Mutex<Value>,
    calls: Counter,
    lists: Counter,
    cancelled: Counter,
    mode: AtomicUsize,
    padding: AtomicUsize,
    replies: Mutex<Vec<Value>>,
    replies_received: Counter,
}
fn tool(name: &str) -> Value {
    json!({"name":name,"description":"A controlled tool","inputSchema":{"type":"object","properties":{"value":{"type":"integer"}},"required":["value"]}})
}
fn server() -> Arc<Server> {
    Arc::new(Server {
        tools: Mutex::new(vec![tool("write")]),
        result: Mutex::new(
            json!({"content":[{"type":"text","text":"done"}],"structuredContent":{"value":1},"_meta":{"private":"secret"}}),
        ),
        calls: Counter::new(0),
        lists: Counter::new(0),
        cancelled: Counter::new(0),
        mode: AtomicUsize::new(0),
        padding: AtomicUsize::new(0),
        replies: Mutex::new(Vec::new()),
        replies_received: Counter::new(0),
    })
}
fn config() -> McpConfig {
    let mut config = McpConfig::new("server", "credential-scope-1");
    config.operation_timeout = Duration::from_millis(500);
    config
}
fn call() -> Call {
    support::call("server.write", json!({"value":1}))
}
fn discover_result() -> Value {
    json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
}
async fn connect(
    server: Arc<Server>,
    config: McpConfig,
) -> (
    McpConnection,
    tokio::sync::mpsc::Sender<Value>,
    CancellationToken,
) {
    let (client, remote) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(client);
    let (reader, mut writer) = tokio::io::split(remote);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(4);
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            let outgoing = tokio::select! {
                value = rx.recv() => match value { Some(value) => value, None => break },
                line = lines.next_line() => {
                    let Ok(Some(line)) = line else { break; };
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let Some(method) = request["method"].as_str() else {
                        server.replies.lock().unwrap().push(request);
                        server.replies_received.fetch_add(1, Ordering::SeqCst);
                        continue;
                    };
                    let result = match method {
                        "server/discover" => {
                            assert!(request["params"]["_meta"].is_object(), "{request}");
                            discover_result()
                        },
                        "tools/list" => {
                            server.lists.fetch_add(1, Ordering::SeqCst);
                            if server.mode.load(Ordering::SeqCst) == 6 { continue; }
                            let mut result = json!({"tools":server.tools.lock().unwrap().clone()});
                            if server.mode.load(Ordering::SeqCst) == 5 { result["nextCursor"] = json!("repeated"); }
                            result
                        },
                        "tools/call" => {
                            assert_eq!(request["params"]["name"], "write");
                            server.calls.fetch_add(1, Ordering::SeqCst);
                            match server.mode.load(Ordering::SeqCst) {
                                1 => continue, // Effect applied; reply lost.
                                2 => { let _ = writer.write_all(&vec![b'x'; 2 * 1024 * 1024]).await; continue; },
                                3 => break,
                                4 => {
                                    let error = json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32603,"message":"secret remote diagnostic","data":{"secret":true}}});
                                    if writer.write_all(format!("{error}\n").as_bytes()).await.is_err() { break; }
                                    continue;
                                }
                                _ => server.result.lock().unwrap().clone(),
                            }
                        },
                        "notifications/cancelled" => { server.cancelled.fetch_add(1, Ordering::SeqCst); continue; },
                        _ => continue,
                    };
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                }
            };
            let mut frame = outgoing.to_string().into_bytes();
            frame.resize(frame.len().max(server.padding.load(Ordering::SeqCst)), b' ');
            frame.push(b'\n');
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    let shutdown = CancellationToken::new();
    (
        McpConnection::stdio(read, write, shutdown.clone(), config)
            .await
            .unwrap(),
        tx,
        shutdown,
    )
}
async fn binding(connection: &McpConnection) -> McpToolBinding {
    connection
        .discover()
        .await
        .unwrap()
        .bind(vec![selected("write", vec![PrincipalKind::Human])])
        .unwrap()
}
async fn wait_count(count: &Counter, expected: usize) {
    count.wait(expected).await;
}
#[tokio::test(start_paused = true)]
async fn principals_provenance_and_results_follow_host_contracts() {
    let server = server();
    server.tools.lock().unwrap()[0]["_meta"] = json!({"principalKinds":["forged"]});
    let (connection, _tx, shutdown) = connect(server.clone(), config()).await;
    let catalogue = connection.discover().await.unwrap();
    assert_eq!(
        catalogue.provenance("write").unwrap()["_meta"]["principalKinds"],
        json!(["forged"])
    );
    let binding = catalogue
        .bind(vec![selected("write", vec![PrincipalKind::Human])])
        .unwrap();
    assert_eq!(
        binding.operations()[0].principals,
        vec![PrincipalKind::Human]
    );
    let result = binding.executor().call(call()).await.unwrap();
    assert!(!format!("{result:?}").contains("secret"));
    let mut invalid = call();
    invalid.input = json!({"value":"bad"});
    assert_eq!(
        binding.executor().call(invalid).await.unwrap_err().evidence,
        Evidence::NotDispatched
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    connection.close();
    shutdown.cancelled().await;
}
#[tokio::test(start_paused = true)]
async fn invalid_tools_duplicates_and_alias_collisions_are_refused() {
    let server = server();
    let mut invalid = tool("invalid");
    invalid["inputSchema"] =
        json!({"type":"object","properties":{"x":{"$ref":"https://invalid.example/schema"}}});
    *server.tools.lock().unwrap() = vec![
        tool("a.b"),
        tool("a_b"),
        invalid,
        tool("duplicate"),
        tool("duplicate"),
    ];
    let (connection, _tx, _) = connect(server, config()).await;
    let catalogue = connection.discover().await.unwrap();
    assert_eq!(
        catalogue
            .diagnostics()
            .iter()
            .map(|d| d.index)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    for name in ["invalid", "duplicate"] {
        assert!(catalogue.bind(vec![McpSelection::new(name)]).is_err());
    }
    // Clapham keeps dots and underscores distinct rather than normalising names.
    let first = catalogue
        .bind(vec![McpSelection::new("a.b"), McpSelection::new("a_b")])
        .unwrap();
    assert_eq!(first.operations().len(), 2);
    let mut alias = McpSelection::new("a_b");
    alias.alias = Some("a.b".into());
    assert!(
        catalogue
            .bind(vec![McpSelection::new("a.b"), alias])
            .is_err()
    );
    assert!(
        clapham::compose(
            [first.contribution(), first.contribution()],
            clapham::Bounds::default()
        )
        .is_err()
    );
}
#[tokio::test(start_paused = true)]
async fn notifications_and_refresh_guard_pinned_definitions() {
    let server = server();
    let (connection, tx, _) = connect(server.clone(), config()).await;
    let binding = binding(&connection).await;
    tx.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}))
        .await
        .unwrap();
    // The following request is read after the notification; its reply acknowledges observation.
    tx.send(json!({"jsonrpc":"2.0","id":100,"method":"ping"}))
        .await
        .unwrap();
    server.replies_received.wait(1).await;
    let error = binding.executor().call(call()).await.unwrap_err();
    assert_eq!(error.evidence, Evidence::NotDispatched);
    connection.discover().await.unwrap();
    binding.executor().call(call()).await.unwrap();
    server.tools.lock().unwrap()[0]["description"] = json!("Changed");
    connection.discover().await.unwrap();
    assert!(binding.executor().call(call()).await.is_err());
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn applied_effect_with_lost_reply_is_uncertain_and_never_replayed() {
    let server = server();
    server.mode.store(1, Ordering::SeqCst);
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let binding = binding(&connection).await;
    let error = binding.executor().call(call()).await.unwrap_err();
    assert_eq!(error.code, "mcp_timeout");
    assert!(error.retryable);
    assert_eq!(error.evidence, Evidence::Uncertain);
    wait_count(&server.cancelled, 1).await;
    connection.close();
    let (reconnected, _tx2, _) = connect(server.clone(), config()).await;
    reconnected.discover().await.unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn dropping_call_requests_cancellation_without_replay() {
    let server = server();
    server.mode.store(1, Ordering::SeqCst);
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let executor = binding(&connection).await.executor();
    let task = tokio::spawn(async move { executor.call(call()).await });
    wait_count(&server.calls, 1).await;
    task.abort();
    let _ = task.await;
    wait_count(&server.cancelled, 1).await;
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn saturated_queue_refuses_without_dispatch() {
    let server = server();
    server.mode.store(1, Ordering::SeqCst);
    let mut config = config();
    config.bounds.in_flight = 1;
    config.bounds.waiting = 0;
    let (connection, _tx, _) = connect(server.clone(), config).await;
    let executor = binding(&connection).await.executor();
    let first = {
        let executor = executor.clone();
        tokio::spawn(async move { executor.call(call()).await })
    };
    wait_count(&server.calls, 1).await;
    let error = executor.call(call()).await.unwrap_err();
    assert_eq!(error.evidence, Evidence::NotDispatched);
    first.abort();
    let _ = first.await;
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn oversized_frames_and_disconnect_preserve_uncertainty() {
    for mode in [2, 3] {
        let server = server();
        server.mode.store(mode, Ordering::SeqCst);
        let (connection, _tx, _) = connect(server.clone(), config()).await;
        let error = binding(&connection)
            .await
            .executor()
            .call(call())
            .await
            .unwrap_err();
        assert_eq!(error.evidence, Evidence::Uncertain);
        if mode == 2 {
            assert!(error.message.contains("limit"));
        }
        assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test(start_paused = true)]
async fn tool_errors_continue_and_protocol_errors_have_bounded_diagnostics() {
    let server = server();
    server.tools.lock().unwrap()[0]["outputSchema"] =
        json!({"type":"object","required":["answer"]});
    *server.result.lock().unwrap() =
        json!({"content":[{"type":"text","text":"Tool failed"}],"isError":true});
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let executor = binding(&connection).await.executor();
    assert!(executor.call(call()).await.unwrap().is_error);
    server.mode.store(4, Ordering::SeqCst);
    let error = executor.call(call()).await.unwrap_err();
    assert_eq!(error.evidence, Evidence::Completed);
    assert!(!format!("{error:?}").contains("secret"));
    server.mode.store(0, Ordering::SeqCst);
    *server.result.lock().unwrap() = json!({"content":[],"structuredContent":{"wrong":1}});
    assert_eq!(
        executor.call(call()).await.unwrap_err().evidence,
        Evidence::Completed
    );
}

#[tokio::test(start_paused = true)]
async fn discovery_bounds_and_failed_refresh_invalidate_existing_bindings() {
    let server = server();
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let binding = binding(&connection).await;
    server.mode.store(5, Ordering::SeqCst);
    assert!(matches!(connection.discover().await, Err(McpError::Limit)));
    assert_eq!(
        binding.executor().call(call()).await.unwrap_err().evidence,
        Evidence::NotDispatched
    );
    server.mode.store(0, Ordering::SeqCst);
    let mut config = config();
    config.bounds.tools = 1;
    server.tools.lock().unwrap().push(tool("second"));
    let (connection, _tx, _) = connect(server, config).await;
    assert!(matches!(connection.discover().await, Err(McpError::Limit)));
}
#[tokio::test(start_paused = true)]
async fn binding_revision_pins_host_principals_and_aliases() {
    let (connection, _tx, _) = connect(server(), config()).await;
    let catalogue = connection.discover().await.unwrap();
    let bind = |selection| catalogue.bind(vec![selection]).unwrap();
    let first = bind(selected("write", vec![PrincipalKind::Human]));
    let second = bind(selected("write", vec![PrincipalKind::Agent]));
    assert_ne!(first.revision(), second.revision());
    let mut selection = selected("write", vec![PrincipalKind::Human]);
    selection.alias = Some("alternate".into());
    assert_ne!(first.revision(), bind(selection).revision());
}
#[tokio::test(start_paused = true)]
async fn unsupported_interactions_are_rejected_without_follow_up_calls() {
    let server = server();
    let (connection, tx, _) = connect(server.clone(), config()).await;
    let executor = binding(&connection).await.executor();
    for (id, method, params) in [
        (1, "roots/list", json!({})),
        (
            2,
            "elicitation/create",
            json!({"message":"Input","requestedSchema":{"type":"object","properties":{}}}),
        ),
        (
            3,
            "sampling/createMessage",
            json!({"messages":[],"maxTokens":1}),
        ),
        (4, "tasks/get", json!({"taskId":"task"})),
    ] {
        tx.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
            .unwrap();
    }
    server.replies_received.wait(4).await;
    {
        let replies = server.replies.lock().unwrap();
        assert!(
            replies
                .iter()
                .all(|r| r["error"]["code"].as_i64().is_some_and(|code| code < 0)
                    && r.get("result").is_none()),
            "{replies:?}"
        );
    }
    *server.result.lock().unwrap() = json!({"resultType":"input_required","inputRequests":{"roots":{"method":"roots/list"}},"requestState":"opaque"});
    let error = executor.call(call()).await.unwrap_err();
    assert_eq!(error.evidence, Evidence::Uncertain);
    assert!(error.message.contains("unsupported"), "{}", error.message);
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn unsupported_revision_is_refused_and_host_shutdown_signalled() {
    let (client, remote) = tokio::io::duplex(4096);
    let (read, write) = tokio::io::split(client);
    tokio::spawn(async {
        let (read, mut write) = tokio::io::split(remote);
        let request: Value = serde_json::from_str(
            &BufReader::new(read)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        let mut result = discover_result();
        result["supportedVersions"] = json!(["2025-03-26"]);
        write
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":request["id"],"result":result})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let shutdown = CancellationToken::new();
    assert!(matches!(
        McpConnection::stdio(read, write, shutdown.clone(), config()).await,
        Err(McpError::Protocol)
    ));
    shutdown.cancelled().await;
}

#[tokio::test(start_paused = true)]
async fn aggregate_queue_capacity_and_notification_limits_are_enforced() {
    let (client, _) = tokio::io::duplex(64);
    let (read, write) = tokio::io::split(client);
    let mut invalid = config();
    invalid.bounds.message_bytes = 8 * 1024 * 1024;
    assert!(matches!(
        McpConnection::stdio(read, write, CancellationToken::new(), invalid).await,
        Err(McpError::Configuration)
    ));
    let mut config = config();
    config.bounds.notifications = 1;
    let (connection, tx, shutdown) = connect(server(), config).await;
    for _ in 0..2 {
        tx.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}))
            .await
            .unwrap();
    }
    shutdown.cancelled().await;
    assert!(matches!(connection.discover().await, Err(McpError::Limit)));
}

#[tokio::test(start_paused = true)]
async fn abandoned_discovery_closes_its_pending_request_state() {
    let server = server();
    server.mode.store(6, Ordering::SeqCst);
    let (connection, _tx, shutdown) = connect(server.clone(), config()).await;
    let copy = connection.clone();
    let task = tokio::spawn(async move { copy.discover().await });
    wait_count(&server.lists, 1).await;
    task.abort();
    let _ = task.await;
    shutdown.cancelled().await;
    assert!(connection.discover().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn duplicate_names_in_a_refresh_invalidate_existing_bindings() {
    let server = server();
    let original = tool("write");
    let mut changed = original.clone();
    changed["description"] = json!("Another definition");
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let executor = binding(&connection).await.executor();
    for records in [
        vec![original.clone(), original.clone()],
        vec![changed, original.clone()],
    ] {
        *server.tools.lock().unwrap() = records;
        let catalogue = connection.discover().await.unwrap();
        assert!(!catalogue.diagnostics().is_empty());
        let error = executor.call(call()).await.unwrap_err();
        assert_eq!(error.evidence, Evidence::NotDispatched);
        assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    }
    *server.tools.lock().unwrap() = vec![original];
    connection.discover().await.unwrap();
    executor.call(call()).await.unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn padded_notifications_exhaust_the_received_byte_budget() {
    let server = server();
    let config = config();
    let frame_bytes = config.bounds.message_bytes;
    let (connection, tx, shutdown) = connect(server.clone(), config).await;
    server.padding.store(frame_bytes - 1, Ordering::SeqCst);
    for _ in 0..65 {
        if tx
            .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}))
            .await
            .is_err()
        {
            break;
        }
    }
    shutdown.cancelled().await;
    assert!(matches!(connection.discover().await, Err(McpError::Limit)));
}

#[tokio::test(start_paused = true)]
async fn message_limits_require_room_for_metadata_and_calls() {
    for bytes in [1, 4096, 8191] {
        let (client, _remote) = tokio::io::duplex(8192);
        let (read, write) = tokio::io::split(client);
        let mut config = config();
        config.bounds.message_bytes = bytes;
        config.bounds.schema_bytes = bytes;
        assert!(matches!(
            McpConnection::stdio(read, write, CancellationToken::new(), config).await,
            Err(McpError::Configuration)
        ));
    }
    let server = server();
    let mut config = config();
    config.bounds.message_bytes = 8192;
    config.bounds.schema_bytes = 4096;
    let (connection, _tx, _) = connect(server.clone(), config).await;
    binding(&connection)
        .await
        .executor()
        .call(call())
        .await
        .unwrap();
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn every_duplicate_record_has_one_source_diagnostic() {
    let server = server();
    let mut invalid = tool("duplicate");
    invalid["inputSchema"] =
        json!({"type":"object","properties":{"x":{"$ref":"https://invalid.example/schema"}}});
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    for first in [tool("duplicate"), invalid] {
        *server.tools.lock().unwrap() =
            vec![first, tool("other"), tool("duplicate"), tool("duplicate")];
        let catalogue = connection.discover().await.unwrap();
        assert_eq!(catalogue.remote_names().collect::<Vec<_>>(), vec!["other"]);
        assert_eq!(
            catalogue
                .diagnostics()
                .iter()
                .map(|d| (d.index, d.name.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (0, Some("duplicate")),
                (2, Some("duplicate")),
                (3, Some("duplicate"))
            ]
        );
    }
}

#[tokio::test(start_paused = true)]
async fn effect_claims_and_principal_authority_remain_separate() {
    use clapham::{Effect, Locality};
    let server = server();
    let mut records = vec![tool("read"), tool("erase"), tool("write")];
    records[0]["annotations"] =
        json!({"readOnlyHint":true,"destructiveHint":true,"openWorldHint":false});
    records[1]["annotations"] =
        json!({"destructiveHint":true,"idempotentHint":true,"openWorldHint":true});
    records[2]["_meta"] =
        json!({"io.github.thirtytwobits.clapham/effect":"administrative","principals":["system"]});
    *server.tools.lock().unwrap() = records;
    let (connection, _tx, _) = connect(server, config()).await;
    let catalogue = connection.discover().await.unwrap();
    let binding = catalogue
        .bind(vec![
            McpSelection::new("read"),
            McpSelection::new("erase"),
            McpSelection::new("write"),
        ])
        .unwrap();
    let operations = binding.operations();
    assert_eq!(
        operations.iter().map(|op| op.effect).collect::<Vec<_>>(),
        vec![Effect::Query, Effect::Destructive, Effect::Mutation]
    );
    assert!(!operations[0].open_world);
    assert!(operations[1].open_world && operations[1].idempotent);
    assert!(operations[2].open_world);
    for operation in operations {
        assert_eq!(operation.locality, Locality::Daemon);
        assert_eq!(operation.principals, operation.effect.default_principals());
        assert!(!operation.preview && !operation.streams);
    }
}

#[test]
fn adapter_refusals_have_stable_categories_and_numeric_diagnostics() {
    use clapham::Category::*;
    for (error, category) in [
        (McpError::Invocation(-32602), InvalidInput),
        (McpError::Invocation(-32601), NotFound),
        (McpError::Invocation(-32000), Internal),
        (McpError::Timeout, Interrupted),
        (McpError::Transport, Unavailable),
        (McpError::Busy, Unavailable),
        (McpError::Limit, Unavailable),
        (McpError::Stale, Conflict),
        (McpError::Configuration, Configuration),
        (McpError::Selection, Usage),
        (McpError::Protocol, Internal),
        (McpError::UnsupportedInteraction, Internal),
    ] {
        for evidence in [
            Evidence::NotDispatched,
            Evidence::Completed,
            Evidence::Uncertain,
        ] {
            let refusal = error.refusal(evidence);
            assert_eq!(refusal.category, category);
            assert_eq!(refusal.evidence, evidence);
            assert_eq!(refusal.retryable, matches!(error, McpError::Timeout));
            assert_eq!(
                refusal.details,
                if let McpError::Invocation(code) = error {
                    Some(json!({"json_rpc_code":code}))
                } else {
                    None
                }
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn binding_and_selection_revisions_fence_metadata_and_credentials() {
    let server = server();
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let first = binding(&connection).await;
    let first_set = clapham::compose([first.contribution()], clapham::Bounds::default()).unwrap();
    server.tools.lock().unwrap()[0]["_meta"] = json!({"revision":2});
    let second = binding(&connection).await;
    let second_set = clapham::compose([second.contribution()], clapham::Bounds::default()).unwrap();
    assert_eq!(first.operations(), second.operations());
    assert_ne!(first.revision(), second.revision());
    assert_ne!(
        first_set.select(["server.write"]).unwrap().revision(),
        second_set.select(["server.write"]).unwrap().revision()
    );
    assert_eq!(
        first_set.select(["clapham.version"]).unwrap().revision(),
        second_set.select(["clapham.version"]).unwrap().revision()
    );
    let mut changed_scope = config();
    changed_scope.credential_scope_revision = "credential-scope-2".into();
    let (other, _other_tx, _) = connect(server, changed_scope).await;
    assert_ne!(second.revision(), binding(&other).await.revision());
}

#[tokio::test(start_paused = true)]
async fn content_order_and_structured_null_survive_without_fetching_references() {
    use clapham::{ContentBlock, ResourceContents};
    let server = server();
    *server.result.lock().unwrap() = json!({"structuredContent":null,"content":[
        {"type":"text","text":"text"}, {"type":"image","data":"YWJj","mimeType":"image/png"},
        {"type":"audio","data":"YWJj","mimeType":"audio/wav"},
        {"type":"resource_link","uri":"https://invalid.example/item","name":"Item"},
        {"type":"resource","resource":{"uri":"lists://tasks","text":"Tasks"}},
        {"type":"resource","resource":{"uri":"lists://binary","blob":"YWJj","mimeType":"application/octet-stream"}}
    ]});
    let (connection, _tx, _) = connect(server.clone(), config()).await;
    let outcome = binding(&connection)
        .await
        .executor()
        .call(call())
        .await
        .unwrap();
    assert_eq!(outcome.result, Some(Value::Null));
    assert_eq!(
        outcome.content.unwrap(),
        vec![
            ContentBlock::Text {
                text: "text".into()
            },
            ContentBlock::Image {
                url: "data:image/png;base64,YWJj".into(),
                mime_type: "image/png".into()
            },
            ContentBlock::Audio {
                url: "data:audio/wav;base64,YWJj".into(),
                mime_type: "audio/wav".into()
            },
            ContentBlock::ResourceLink {
                uri: "https://invalid.example/item".into(),
                name: "Item".into(),
                mime_type: None
            },
            ContentBlock::Resource {
                uri: "lists://tasks".into(),
                mime_type: None,
                contents: ResourceContents::Text("Tasks".into())
            },
            ContentBlock::Resource {
                uri: "lists://binary".into(),
                mime_type: Some("application/octet-stream".into()),
                contents: ResourceContents::Base64("YWJj".into())
            },
        ]
    );
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
}
