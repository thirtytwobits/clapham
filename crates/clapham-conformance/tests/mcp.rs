//! Deterministic connection, cancellation and byte-boundary coverage.

use std::sync::Arc;

use clapham::{
    Bounds, Call, ComposeOptions, Contribution, OAuthContext, Operation, Outcome, compose,
    compose_with_options,
};
use clapham_conformance::{ListsGate, ListsHandle};
use clapham_mcp_server::Server;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct Wire {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    task: tokio::task::JoinHandle<()>,
}
impl Wire {
    fn new(server: Server) -> Self {
        let (client, transport) = tokio::io::duplex(1024);
        let (read, write) = tokio::io::split(transport);
        let task = tokio::spawn(async move {
            server
                .serve_io(read, write)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let (read, input) = tokio::io::split(client);
        Self {
            input,
            output: BufReader::new(read),
            task,
        }
    }
    async fn send(&mut self, id: u64, method: &str, mut params: Value) {
        let meta = params
            .as_object_mut()
            .unwrap()
            .entry("_meta")
            .or_insert(json!({}));
        meta["io.modelcontextprotocol/protocolVersion"] = json!("2026-07-28");
        meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
        self.raw(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
    }
    async fn raw(&mut self, message: Value) {
        self.input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn response(&mut self) -> Value {
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).await.unwrap(),
            0,
            "connection closed"
        );
        serde_json::from_str(&line).unwrap()
    }
    async fn result(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(id, method, params).await;
        let response = self.response().await;
        assert_eq!(response["id"], id);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    async fn close(mut self) {
        self.input.shutdown().await.unwrap();
        drop(self.input);
        drop(self.output);
        self.task.await.unwrap();
    }
}

fn auth() -> OAuthContext {
    oauth_context(
        "launcher",
        &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
    )
}
fn fixture() -> (ListsHandle, Server) {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    let set = compose_with_options(
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
        Server::new(set, auth())
            .unwrap()
            .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
            .unwrap(),
    )
}
fn descriptor(name: &str) -> Operation {
    serde_json::from_value(json!({"name":name,"description":"Probe a protocol boundary.","input":{"type":"object","additionalProperties":false},"effect":"query","locality":"workspace","idempotent":true,"open_world":false})).unwrap()
}

#[tokio::test]
async fn selection_limits_tools_resources_and_direct_calls_without_exposing_describe() {
    let (_, server) = fixture();
    let mut wire = Wire::new(server.select(["lists.get"]).unwrap());
    let tools = wire.result(1, "tools/list", json!({})).await;
    assert_eq!(tools["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tools["tools"][0]["name"], "lists.get");
    let denied = wire
        .result(
            2,
            "tools/call",
            json!({"name":"lists.create","arguments":{"list":{"id":"extra","name":"Extra"}}}),
        )
        .await;
    assert_eq!(denied["structuredContent"]["code"], "unknown_operation");
    let hidden = wire
        .result(3, "tools/call", json!({"name":"clapham.describe"}))
        .await;
    assert_eq!(hidden["structuredContent"]["code"], "unknown_operation");
    let got = wire
        .result(
            4,
            "tools/call",
            json!({"name":"lists.get","arguments":{"id":"tasks"}}),
        )
        .await;
    assert_eq!(got["structuredContent"]["id"], "tasks");
    let missing = wire.result(5, "resources/list", json!({})).await;
    assert_eq!(missing["resources"], json!([]));
    wire.close().await;
}

#[tokio::test]
async fn cancelled_held_call_releases_execution_and_transport_slots() {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    let set = compose(
        handle.contributions(),
        Bounds {
            in_flight: 1,
            waiting: 0,
            ..Bounds::default()
        },
    )
    .unwrap();
    let server = Server::new(set, auth()).unwrap();
    let mut held = handle.hold("lists.get").unwrap();
    let mut wire = Wire::new(server);
    wire.send(
        1,
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"tasks"}}),
    )
    .await;
    assert!(held.entered().await);
    wire.raw(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"reason":"test"}})).await;
    // MCP cancellation suppresses the first reply. A second request can run
    // only once both the transport and execution slots have been released.
    let next = wire
        .result(
            2,
            "tools/call",
            json!({"name":"lists.get","arguments":{"id":"tasks"}}),
        )
        .await;
    assert_eq!(next["structuredContent"]["id"], "tasks");
    assert!(!held.release(), "cancelled handler future was dropped");
    wire.close().await;
}

#[tokio::test]
async fn constant_resource_and_missing_resource_have_json_contents() {
    let (_, server) = fixture();
    let mut wire = Wire::new(server);
    let all = wire
        .result(1, "resources/read", json!({"uri":"lists://"}))
        .await;
    assert_eq!(
        serde_json::from_str::<Value>(all["contents"][0]["text"].as_str().unwrap())
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let missing = wire
        .result(2, "resources/read", json!({"uri":"lists://missing"}))
        .await;
    let refusal: Value =
        serde_json::from_str(missing["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refusal["code"], "list_not_found");
    wire.send(3, "resources/read", json!({"uri":"lists://bad%ZZ"}))
        .await;
    assert!(wire.response().await["error"].is_object());
    wire.close().await;
}

#[tokio::test]
async fn pagination_is_bounded_and_cursors_are_checked() {
    let mut contribution = Contribution::new("probe");
    for index in 0..12 {
        let mut op = descriptor(&format!("probe.get{index:02}"));
        op.description = "x".repeat(3000);
        contribution =
            contribution.operation(op, |_: Call| async { Ok(Outcome::new(json!(null))) });
    }
    let bounds = Bounds {
        message_bytes: 8192,
        schema_bytes: 8192,
        ..Bounds::default()
    };
    let set = compose([contribution], bounds).unwrap();
    let mut wire = Wire::new(Server::new(set, auth()).unwrap());
    let mut cursor = None;
    let mut names = Vec::new();
    let mut pages = 0;
    loop {
        pages += 1;
        let page = wire
            .result(pages, "tools/list", json!({"cursor":cursor}))
            .await;
        assert!(page.to_string().len() < 8192);
        names.extend(
            page["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_owned()),
        );
        cursor = page.get("nextCursor").cloned();
        if cursor.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(names.len(), 14);
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted, names);
    wire.send(90, "tools/list", json!({"cursor":"invalid"}))
        .await;
    assert_eq!(wire.response().await["error"]["code"], -32602);
    wire.close().await;
}

#[tokio::test]
async fn frame_limits_include_encoded_result_and_oversized_input_recovers() {
    let op = descriptor("probe.large");
    let contribution = Contribution::new("probe").operation(op, |_: Call| async {
        Ok(Outcome::new(json!({"text":"x".repeat(8192)})))
    });
    let set = compose(
        [contribution],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let mut wire = Wire::new(Server::new(set, auth()).unwrap());
    // Bounded decoding discards an entire oversized line, then resumes at the next.
    wire.input
        .write_all(format!("{}\n", "x".repeat(16384)).as_bytes())
        .await
        .unwrap();
    let result = wire
        .result(1, "tools/call", json!({"name":"probe.large"}))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["code"], "result_too_large");
    assert_eq!(result["structuredContent"]["evidence"], "uncertain");
    wire.close().await;
}

#[tokio::test]
async fn content_null_and_boolean_output_schema_retain_their_meaning() {
    let mut op = descriptor("probe.content");
    op.title = Some("Contents".into());
    op.output = Some(true.into());
    let contribution = Contribution::new("probe").operation(op, |_: Call| async {
        Ok(Outcome {
            result: Some(Value::Null),
            content: Some(vec![
                json!({"type":"text","text":"Text"}),
                json!({"type":"image","data":"AAAA","mimeType":"image/png"}),
                json!({"type":"resource_link","uri":"https://example.invalid/audio","name":"https://example.invalid/audio","mimeType":"audio/wav"}),
            ]),
            is_error: true,
            evidence: clapham::Evidence::Completed,
        })
    });
    let set = compose([contribution], Bounds::default()).unwrap();
    let mut wire = Wire::new(Server::new(set, auth()).unwrap());
    let catalogue = wire.result(1, "tools/list", json!({})).await;
    let tool = catalogue["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "probe.content")
        .unwrap();
    assert_eq!(tool["title"], "Contents");
    assert_eq!(tool["outputSchema"], json!({}));
    let result = wire
        .result(2, "tools/call", json!({"name":"probe.content"}))
        .await;
    assert_eq!(result.get("structuredContent"), Some(&Value::Null));
    assert_eq!(result["isError"], true);
    assert_eq!(result["content"][1]["data"], "AAAA");
    assert_eq!(result["content"][2]["type"], "resource_link");
    wire.close().await;
}

#[tokio::test]
async fn oversized_refusal_closes_instead_of_changing_its_code() {
    let mut op = descriptor("probe.refusal");
    op.refusals
        .insert("original_code".into(), clapham::Category::Conflict);
    let contribution = Contribution::new("probe").operation(op, |_: Call| async {
        Err(clapham::Refusal::new(
            "original_code",
            clapham::Category::Conflict,
            "x".repeat(8192),
        ))
    });
    let set = compose(
        [contribution],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let mut wire = Wire::new(Server::new(set, auth()).unwrap());
    wire.send(1, "tools/call", json!({"name":"probe.refusal"}))
        .await;
    let mut line = String::new();
    assert_eq!(wire.output.read_line(&mut line).await.unwrap(), 0);
    wire.close().await;
}

#[test]
fn unsupported_resource_templates_refuse_before_serving() {
    for uri in ["lists://{+id}", "lists://{id}{name}", "lists://{id}.json"] {
        let mut op = descriptor("probe.resource");
        op.input =
            json!({"type":"object","properties":{"id":{"type":"string"},"name":{"type":"string"}}})
                .as_object()
                .unwrap()
                .clone();
        let contribution = Contribution::new("probe")
            .operation(op, |_: Call| async { Ok(Outcome::new(Value::Null)) });
        let set = compose([contribution], Bounds::default()).unwrap();
        assert_eq!(
            Server::new(set, auth())
                .unwrap()
                .resources([("probe.resource", uri)])
                .unwrap_err()
                .code,
            "mcp_configuration"
        );
    }
}

#[tokio::test]
async fn describe_admission_is_required_for_discovery_but_does_not_disable_calls() {
    for (decision, code) in [
        (clapham::Admission::Deny, "not_admitted"),
        (clapham::Admission::Pending, "admission_pending"),
    ] {
        let handle = ListsHandle::new();
        handle.create_list("tasks", "Tasks").unwrap();
        let set = compose_with_options(
            handle.contributions(),
            Bounds::default(),
            ComposeOptions {
                gate: Arc::new(move |_: &OAuthContext, op: &Operation| {
                    if op.name.as_str() == "clapham.describe" {
                        decision
                    } else {
                        clapham::Admission::Allow
                    }
                }),
                ..ComposeOptions::default()
            },
        )
        .unwrap();
        let mut wire = Wire::new(
            Server::new(set, auth())
                .unwrap()
                .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
                .unwrap()
                .select(["lists.get"])
                .unwrap(),
        );
        for (index, method) in [
            "server/discover",
            "tools/list",
            "resources/list",
            "resources/templates/list",
            "resources/read",
        ]
        .into_iter()
        .enumerate()
        {
            wire.send(
                index as u64,
                method,
                if method == "resources/read" {
                    json!({"uri":"lists://tasks"})
                } else {
                    json!({})
                },
            )
            .await;
            let response = wire.response().await;
            assert_eq!(
                response["error"]["data"]["code"], code,
                "{method}: {response}"
            );
        }
        let result = wire
            .result(
                10,
                "tools/call",
                json!({"name":"lists.get","arguments":{"id":"tasks"}}),
            )
            .await;
        assert_eq!(result["structuredContent"]["name"], "Tasks");
        wire.close().await;
    }
}

#[tokio::test]
async fn oversized_resource_result_returns_refusal_and_keeps_connection_usable() {
    let op = descriptor("probe.large");
    let contribution = Contribution::new("probe").operation(op, |_: Call| async {
        Ok(Outcome::new(json!({"text":"x".repeat(8192)})))
    });
    let set = compose(
        [contribution],
        Bounds {
            message_bytes: 8192,
            schema_bytes: 8192,
            ..Bounds::default()
        },
    )
    .unwrap();
    let mut wire = Wire::new(
        Server::new(set, auth())
            .unwrap()
            .resources([("probe.large", "probe://large")])
            .unwrap(),
    );
    let result = wire
        .result(1, "resources/read", json!({"uri":"probe://large"}))
        .await;
    let refusal: Value =
        serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(refusal["code"], "result_too_large");
    assert_eq!(refusal["category"], "usage");
    assert_eq!(refusal["evidence"], "uncertain");
    let next = wire
        .result(2, "tools/call", json!({"name":"clapham.version"}))
        .await;
    assert_eq!(next["isError"], false);
    wire.close().await;
}

#[tokio::test]
async fn resource_error_outcomes_retain_content_and_error_flag() {
    let op = descriptor("probe.error");
    let contribution = Contribution::new("probe").operation(op, |_: Call| async {
        Ok(Outcome {
            result: Some(json!({"reason":"unavailable"})),
            content: Some(vec![json!({"type":"text","text":"Details"})]),
            is_error: true,
            evidence: clapham::Evidence::Completed,
        })
    });
    let set = compose([contribution], Bounds::default()).unwrap();
    let mut wire = Wire::new(
        Server::new(set, auth())
            .unwrap()
            .resources([("probe.error", "probe://error")])
            .unwrap(),
    );
    let result = wire
        .result(1, "resources/read", json!({"uri":"probe://error"}))
        .await;
    let outcome: Value =
        serde_json::from_str(result["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(outcome["is_error"], true);
    assert_eq!(outcome["result"]["reason"], "unavailable");
    assert_eq!(outcome["content"][0]["text"], "Details");
    wire.close().await;
}

#[tokio::test]
async fn cancelled_request_id_cannot_be_reused_on_the_connection() {
    let (handle, server) = fixture();
    let mut held = handle.hold("lists.get").unwrap();
    let mut wire = Wire::new(server);
    wire.send(
        1,
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"tasks"}}),
    )
    .await;
    assert!(held.entered().await);
    wire.raw(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}))
        .await;
    wire.send(1, "tools/call", json!({"name":"clapham.version"}))
        .await;
    let mut line = String::new();
    assert_eq!(wire.output.read_line(&mut line).await.unwrap(), 0);
    assert!(!held.release());
    wire.close().await;
}

#[test]
fn equivalent_resource_template_shapes_refuse_before_serving() {
    let mut contribution = Contribution::new("probe");
    for (name, variable) in [("probe.first", "id"), ("probe.second", "name")] {
        let mut op = descriptor(name);
        op.input = json!({"type":"object","properties":{variable:{"type":"string"}}})
            .as_object()
            .unwrap()
            .clone();
        contribution =
            contribution.operation(op, |_: Call| async { Ok(Outcome::new(Value::Null)) });
    }
    let set = compose([contribution], Bounds::default()).unwrap();
    assert_eq!(
        Server::new(set, auth())
            .unwrap()
            .resources([
                ("probe.first", "probe://{id}"),
                ("probe.second", "probe://{name}")
            ])
            .unwrap_err()
            .code,
        "mcp_configuration"
    );
}

#[tokio::test]
async fn resource_discovery_does_not_describe_unrelated_tools() {
    use std::sync::Mutex;
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    let set = compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, op: &Operation| {
                seen.lock().unwrap().push(op.name.to_string());
                clapham::Admission::Allow
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let mut wire = Wire::new(
        Server::new(set, auth())
            .unwrap()
            .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
            .unwrap(),
    );
    wire.result(1, "resources/read", json!({"uri":"lists://tasks"}))
        .await;
    assert!(
        calls
            .lock()
            .unwrap()
            .iter()
            .all(|name| ["clapham.describe", "lists.list", "lists.get"].contains(&name.as_str()))
    );
    wire.close().await;
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}
