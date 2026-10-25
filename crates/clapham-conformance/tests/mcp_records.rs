//! The records extension over the MCP server's stdio and Streamable HTTP transports.

#[path = "support/http.rs"]
mod support;

use std::{
    collections::VecDeque,
    convert::Infallible,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use bytes::Bytes;
use clapham::{
    Bounds, Call, ComposedSet, Configuration, Contribution, Effect, Exposure, OAuthContext,
    TypedOperation,
};
use clapham_mcp_server::{
    ACKNOWLEDGE_REQUEST, HttpServer, RECORD_NOTIFICATION, RECORDS_EXTENSION, Server,
};
use http::{StatusCode, header};
use http_body_util::{BodyExt, combinators::BoxBody};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf},
    sync::oneshot,
};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Export {
    count: u64,
    #[serde(default)]
    pad: usize,
    /// Replace a record refused as too large with an unpadded one.
    #[serde(default)]
    shrink: bool,
}

/// Fires when the handler's future is dropped, including by cancellation.
struct Dropped(Option<oneshot::Sender<()>>);
impl Drop for Dropped {
    fn drop(&mut self) {
        if let Some(dropped) = self.0.take() {
            let _ = dropped.send(());
        }
    }
}

/// `items.export` sends `count` records; `returned` counts the sends that have returned.
struct Script {
    returned: Arc<AtomicUsize>,
    entered: Arc<AtomicUsize>,
    dropped: oneshot::Receiver<()>,
}

fn set(bounds: Bounds) -> (ComposedSet, Script) {
    let returned = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let (dropped, on_drop) = oneshot::channel();
    let dropped = Arc::new(Mutex::new(Some(dropped)));
    let export = TypedOperation::<Export, u64>::new(
        "items.export",
        "Export items as records.",
        Effect::Query,
    )
    .unwrap()
    .streams(true);
    let script = Script {
        returned: returned.clone(),
        entered: entered.clone(),
        dropped: on_drop,
    };
    let provider =
        Contribution::new("items").typed_operation(export, move |call: Call, input: Export| {
            let (returned, entered) = (returned.clone(), entered.clone());
            let guard = Dropped(dropped.lock().unwrap().take());
            async move {
                let _guard = guard;
                entered.fetch_add(1, Ordering::SeqCst);
                for sequence in 1..=input.count {
                    let padded = json!({"item":sequence,"pad":"x".repeat(input.pad)});
                    match call.records.send(padded).await {
                        Err(refusal) if input.shrink && refusal.code == "result_too_large" => {
                            call.records.send(json!({"item":sequence,"pad":""})).await?
                        }
                        sent => sent?,
                    }
                    returned.fetch_add(1, Ordering::SeqCst);
                }
                Ok(input.count)
            }
        });
    let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .provider("items", provider)
        .expose(Exposure::new("items", "items.export"))
        .compose(bounds)
        .unwrap();
    (set, script)
}

/// One streaming call holds the only execution slot; acknowledgements must still arrive.
fn held_bounds() -> Bounds {
    Bounds {
        in_flight: 1,
        waiting: 0,
        ..Bounds::default()
    }
}

fn declaring() -> Value {
    json!({"extensions":{RECORDS_EXTENSION:{}}})
}

fn export(count: u64, token: Option<&str>, capabilities: Value) -> Value {
    let mut meta = json!({"io.modelcontextprotocol/clientCapabilities":capabilities});
    if let Some(token) = token {
        meta[RECORDS_EXTENSION] = json!(token);
    }
    json!({"name":"items.export","arguments":{"count":count},"_meta":meta})
}

fn acknowledgement(token: &str, sequence: u64) -> Value {
    json!({"token":token,"sequence":sequence,"_meta":{
        "io.modelcontextprotocol/clientCapabilities":declaring(),
    }})
}

fn assert_record(message: &Value, token: &str, sequence: u64) {
    assert_eq!(message["method"], RECORD_NOTIFICATION, "{message}");
    assert_eq!(message["params"]["token"], token, "{message}");
    assert_eq!(message["params"]["sequence"], sequence, "{message}");
    assert_eq!(message["params"]["record"]["item"], sequence, "{message}");
    assert!(message.get("id").is_none(), "{message}");
}

fn verified(subject: &str) -> OAuthContext {
    OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        ["items:read"],
    )
}

/// A stdio connection whose replies and records may interleave; unmatched messages wait.
struct Wire {
    input: WriteHalf<DuplexStream>,
    output: BufReader<ReadHalf<DuplexStream>>,
    pending: VecDeque<Value>,
    task: tokio::task::JoinHandle<()>,
}
impl Wire {
    fn new(server: Server) -> Self {
        let (client, transport) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(transport);
        let task = tokio::spawn(async move {
            let _ = server.serve_io(read, write).await.unwrap().waiting().await;
        });
        let (read, input) = tokio::io::split(client);
        Self {
            input,
            output: BufReader::new(read),
            pending: VecDeque::new(),
            task,
        }
    }
    async fn send(&mut self, id: Option<u64>, method: &str, mut params: Value) {
        let meta = params
            .as_object_mut()
            .unwrap()
            .entry("_meta")
            .or_insert(json!({}));
        meta["io.modelcontextprotocol/protocolVersion"] = json!("2026-07-28");
        if meta
            .get("io.modelcontextprotocol/clientCapabilities")
            .is_none()
        {
            meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
        }
        let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
        if let Some(id) = id {
            message["id"] = json!(id);
        }
        self.input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }
    /// The first message, buffered or read, that satisfies `wanted`.
    async fn take(&mut self, wanted: impl Fn(&Value) -> bool) -> Value {
        if let Some(index) = self.pending.iter().position(&wanted) {
            return self.pending.remove(index).unwrap();
        }
        loop {
            let mut line = String::new();
            assert_ne!(
                self.output.read_line(&mut line).await.unwrap(),
                0,
                "connection closed"
            );
            let message: Value = serde_json::from_str(&line).unwrap();
            if wanted(&message) {
                return message;
            }
            self.pending.push_back(message);
        }
    }
    async fn record(&mut self) -> Value {
        self.take(|message| message["method"] == RECORD_NOTIFICATION)
            .await
    }
    async fn reply(&mut self, id: u64) -> Value {
        self.take(|message| message["id"] == id && message.get("method").is_none())
            .await
    }
    async fn acknowledge(&mut self, id: u64, token: &str, sequence: u64) -> Value {
        self.send(
            Some(id),
            ACKNOWLEDGE_REQUEST,
            acknowledgement(token, sequence),
        )
        .await;
        self.reply(id).await
    }
    async fn close(mut self) {
        assert!(self.pending.is_empty(), "unexpected {:?}", self.pending);
        self.input.shutdown().await.unwrap();
        drop(self.input);
        drop(self.output);
        self.task.await.unwrap();
    }
}

#[tokio::test]
async fn records_arrive_in_order_each_acknowledged_before_the_result_over_stdio() {
    let (set, script) = set(held_bounds());
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    wire.send(
        Some(1),
        "tools/call",
        export(2, Some("call-1"), declaring()),
    )
    .await;
    assert_record(&wire.record().await, "call-1", 1);
    // The first send cannot return before its acknowledgement.
    assert_eq!(script.returned.load(Ordering::SeqCst), 0);
    assert_eq!(wire.acknowledge(2, "call-1", 1).await["result"], json!({}));
    assert_record(&wire.record().await, "call-1", 2);
    assert_eq!(script.returned.load(Ordering::SeqCst), 1);
    assert_eq!(wire.acknowledge(3, "call-1", 2).await["result"], json!({}));
    let result = wire.reply(1).await;
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(result["result"]["structuredContent"], 2);
    wire.close().await;
}

#[tokio::test]
async fn a_client_without_the_extension_receives_the_result_alone_over_stdio() {
    let (set, _script) = set(Bounds::default());
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    // A token without the declared extension, then the extension without a token.
    wire.send(Some(1), "tools/call", export(2, Some("call-1"), json!({})))
        .await;
    let result = wire.take(|_| true).await;
    assert_eq!(result["id"], 1);
    assert_eq!(result["result"]["structuredContent"], 2);
    wire.send(Some(2), "tools/call", export(2, None, declaring()))
        .await;
    let result = wire.take(|_| true).await;
    assert_eq!(result["id"], 2);
    assert_eq!(result["result"]["structuredContent"], 2);
    wire.close().await;
}

#[tokio::test]
async fn acknowledgements_are_idempotent_and_bound_to_the_record_sent_over_stdio() {
    let (set, _script) = set(held_bounds());
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    wire.send(
        Some(1),
        "tools/call",
        export(2, Some("call-1"), declaring()),
    )
    .await;
    assert_record(&wire.record().await, "call-1", 1);
    for (id, token, sequence) in [(2, "call-1", 2), (3, "call-1", 0), (4, "other", 1)] {
        let refused = wire.acknowledge(id, token, sequence).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
    }
    assert_eq!(wire.acknowledge(5, "call-1", 1).await["result"], json!({}));
    assert_record(&wire.record().await, "call-1", 2);
    // The reply to the first acknowledgement may be lost; repeating it succeeds.
    assert_eq!(wire.acknowledge(6, "call-1", 1).await["result"], json!({}));
    assert_eq!(wire.acknowledge(7, "call-1", 2).await["result"], json!({}));
    assert_eq!(wire.reply(1).await["result"]["structuredContent"], 2);
    wire.close().await;
}

#[tokio::test]
async fn a_records_token_in_use_refuses_another_call_over_stdio() {
    let (set, _script) = set(Bounds::default());
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    wire.send(
        Some(1),
        "tools/call",
        export(1, Some("call-1"), declaring()),
    )
    .await;
    assert_record(&wire.record().await, "call-1", 1);
    wire.send(
        Some(2),
        "tools/call",
        export(1, Some("call-1"), declaring()),
    )
    .await;
    assert_eq!(wire.reply(2).await["error"]["code"], -32602);
    assert_eq!(wire.acknowledge(3, "call-1", 1).await["result"], json!({}));
    assert_eq!(wire.reply(1).await["result"]["structuredContent"], 1);
    wire.close().await;
}

#[tokio::test]
async fn an_oversized_record_refuses_the_call_with_uncertain_evidence() {
    let bytes = 64 * 1024;
    let (set, _script) = set(Bounds {
        message_bytes: bytes,
        schema_bytes: bytes,
        ..Bounds::default()
    });
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    let mut call = export(1, Some("call-1"), declaring());
    call["arguments"]["pad"] = json!(bytes);
    wire.send(Some(1), "tools/call", call).await;
    let result = wire.take(|_| true).await;
    assert_eq!(result["id"], 1);
    let refusal = &result["result"]["structuredContent"];
    assert_eq!(refusal["code"], "result_too_large", "{result}");
    assert_eq!(refusal["evidence"], "uncertain");
    wire.close().await;
}

#[tokio::test]
async fn a_record_sent_after_an_oversized_one_keeps_the_sequence() {
    let bytes = 64 * 1024;
    let (set, _script) = set(Bounds {
        message_bytes: bytes,
        schema_bytes: bytes,
        ..Bounds::default()
    });
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    let mut call = export(1, Some("call-1"), declaring());
    call["arguments"]["pad"] = json!(bytes);
    call["arguments"]["shrink"] = json!(true);
    wire.send(Some(1), "tools/call", call).await;
    // The refused record was never sent, so the replacement is the first.
    let record = wire.record().await;
    assert_record(&record, "call-1", 1);
    assert_eq!(record["params"]["record"]["pad"], "");
    assert_eq!(wire.acknowledge(2, "call-1", 1).await["result"], json!({}));
    assert_eq!(wire.reply(1).await["result"]["structuredContent"], 1);
    wire.close().await;
}

#[tokio::test]
async fn cancelling_a_call_awaiting_acknowledgement_drops_its_handler_over_stdio() {
    let (set, script) = set(held_bounds());
    let mut wire = Wire::new(Server::new(set, verified("launcher")).unwrap());
    wire.send(
        Some(1),
        "tools/call",
        export(2, Some("call-1"), declaring()),
    )
    .await;
    assert_record(&wire.record().await, "call-1", 1);
    wire.send(None, "notifications/cancelled", json!({"requestId":1}))
        .await;
    script.dropped.await.unwrap();
    assert_eq!(script.returned.load(Ordering::SeqCst), 0);
    wire.close().await;
}

/// JSON-RPC messages from the `data` fields of an SSE reply, read as the server writes them.
struct Events {
    body: BoxBody<Bytes, Infallible>,
    buffer: String,
}
impl Events {
    fn new(response: http::Response<BoxBody<Bytes, Infallible>>) -> Self {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        Self {
            body: response.into_body(),
            buffer: String::new(),
        }
    }
    async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..end + 2).collect();
                let data: Vec<&str> = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(|data| data.strip_prefix(' ').unwrap_or(data))
                    .collect();
                if !data.is_empty() {
                    return Some(serde_json::from_str(&data.join("\n")).unwrap());
                }
                continue;
            }
            let frame = self.body.frame().await?.unwrap();
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }
}

fn http_server(bounds: Bounds) -> (HttpServer, Script) {
    let (set, script) = set(bounds);
    (HttpServer::new(set, support::authenticate).unwrap(), script)
}

async fn reply(response: http::Response<BoxBody<Bytes, Infallible>>) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[tokio::test]
async fn records_stream_before_the_result_while_the_call_holds_every_permit_over_http() {
    let (server, script) = http_server(held_bounds());
    let call = server
        .handle(support::request(
            "tools/call",
            export(2, Some("call-1"), declaring()),
            "Bearer human",
        ))
        .await;
    let mut events = Events::new(call);
    assert_record(&events.next().await.unwrap(), "call-1", 1);
    assert_eq!(script.returned.load(Ordering::SeqCst), 0);
    for sequence in [1, 2] {
        let acknowledged = server
            .handle(support::request(
                ACKNOWLEDGE_REQUEST,
                acknowledgement("call-1", sequence),
                "Bearer human",
            ))
            .await;
        assert_eq!(acknowledged.status(), StatusCode::OK);
        assert_eq!(reply(acknowledged).await["result"], json!({}));
        if sequence == 1 {
            assert_record(&events.next().await.unwrap(), "call-1", 2);
        }
    }
    let result = events.next().await.unwrap();
    assert_eq!(result["result"]["structuredContent"], 2, "{result}");
    assert!(events.next().await.is_none());
    assert_eq!(script.returned.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn acknowledgements_from_another_grant_are_refused_over_http() {
    let (server, _script) = http_server(held_bounds());
    let call = server
        .handle(support::request(
            "tools/call",
            export(1, Some("call-1"), declaring()),
            "Bearer human",
        ))
        .await;
    let mut events = Events::new(call);
    assert_record(&events.next().await.unwrap(), "call-1", 1);
    let refused = server
        .handle(support::request(
            ACKNOWLEDGE_REQUEST,
            acknowledgement("call-1", 1),
            "Bearer agent",
        ))
        .await;
    assert_eq!(reply(refused).await["error"]["code"], -32602);
    let acknowledged = server
        .handle(support::request(
            ACKNOWLEDGE_REQUEST,
            acknowledgement("call-1", 1),
            "Bearer human",
        ))
        .await;
    assert_eq!(reply(acknowledged).await["result"], json!({}));
    assert_eq!(
        events.next().await.unwrap()["result"]["structuredContent"],
        1
    );
}

#[tokio::test]
async fn the_acknowledgement_path_answers_nothing_else_over_http() {
    let (server, script) = http_server(held_bounds());
    let mut request = support::request(
        "tools/call",
        export(1, Some("call-1"), declaring()),
        "Bearer human",
    );
    request.headers_mut().insert(
        "mcp-method",
        http::HeaderValue::from_static(ACKNOWLEDGE_REQUEST),
    );
    request.headers_mut().remove("mcp-protocol-version");
    let response = server.handle(request).await;
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(script.entered.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn closing_the_stream_cancels_a_call_awaiting_acknowledgement_over_http() {
    let (server, script) = http_server(held_bounds());
    let call = server
        .handle(support::request(
            "tools/call",
            export(2, Some("call-1"), declaring()),
            "Bearer human",
        ))
        .await;
    let mut events = Events::new(call);
    assert_record(&events.next().await.unwrap(), "call-1", 1);
    drop(events);
    script.dropped.await.unwrap();
    assert_eq!(script.returned.load(Ordering::SeqCst), 0);
    // The released permit admits the next call.
    let result = support::result(
        &server,
        "tools/call",
        json!({"name":"items.export","arguments":{"count":0}}),
        "Bearer human",
    )
    .await;
    assert_eq!(result["structuredContent"], 0);
}
