//! Records routed to a bound tool's sink, each acknowledged after the sink accepts it.
#![cfg(all(feature = "stdio", feature = "http"))]
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use clapham::{Category, Evidence, RecordSink, Refusal};
use clapham_mcp_client::*;
mod support;
use support::*;

use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio_util::sync::CancellationToken;

const RECORDS: &str = "io.github.thirtytwobits.clapham/records";
const RECORD: &str = "io.github.thirtytwobits.clapham/record";
const ACKNOWLEDGE: &str = "io.github.thirtytwobits.clapham/acknowledge";

fn discovered() -> Value {
    json!({"resultType":"complete","supportedVersions":["2026-07-28"],
        "capabilities":{"tools":{},"extensions":{RECORDS:{}}},"ttlMs":0,"cacheScope":"private"})
}

fn tools() -> Value {
    json!({"tools":[{"name":"export","description":"Export items.",
        "inputSchema":{"type":"object"},"_meta":{RECORDS:true}}]})
}

fn record(token: &str, sequence: u64) -> Value {
    json!({"jsonrpc":"2.0","method":RECORD,
        "params":{"token":token,"sequence":sequence,"record":{"item":sequence}}})
}

/// The test plays the server at the other end of a stdio pipe.
struct Remote {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
}
impl Remote {
    async fn connect() -> (McpConnection, Self) {
        Self::connect_with(McpConfig::new("remote", "scope")).await
    }
    async fn connect_with(config: McpConfig) -> (McpConnection, Self) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(client);
        let connecting = tokio::spawn(McpConnection::stdio(
            read,
            write,
            CancellationToken::new(),
            config,
        ));
        let (read, write) = tokio::io::split(server);
        let mut remote = Self {
            lines: BufReader::new(read).lines(),
            write,
        };
        let discover = remote.receive().await;
        assert_eq!(discover["method"], "server/discover");
        remote.reply(&discover, discovered()).await;
        (connecting.await.unwrap().unwrap(), remote)
    }
    async fn bind(&mut self, connection: &McpConnection) -> McpToolBinding {
        let (catalogue, ()) = tokio::join!(connection.discover(), async {
            let list = self.receive().await;
            assert_eq!(list["method"], "tools/list");
            self.reply(&list, tools()).await;
        });
        catalogue
            .unwrap()
            .bind(vec![McpSelection::new("export")])
            .unwrap()
    }
    async fn receive(&mut self) -> Value {
        let line = self.lines.next_line().await.unwrap().expect("client open");
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, message: Value) {
        self.write
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
    }
    async fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"jsonrpc":"2.0","id":request["id"],"result":result}))
            .await;
    }
    /// The call request and its records token.
    async fn call(&mut self) -> (Value, String) {
        let request = self.receive().await;
        assert_eq!(request["method"], "tools/call");
        let meta = &request["params"]["_meta"];
        assert!(
            meta["io.modelcontextprotocol/clientCapabilities"]["extensions"][RECORDS].is_object(),
            "{request}"
        );
        let token = meta[RECORDS].as_str().unwrap().to_owned();
        (request, token)
    }
    async fn acknowledgement(&mut self, token: &str, sequence: u64) -> Value {
        let request = self.receive().await;
        assert_eq!(request["method"], ACKNOWLEDGE, "{request}");
        assert_eq!(request["params"]["token"], token);
        assert_eq!(request["params"]["sequence"], sequence);
        request
    }
    async fn cancelled(&mut self, request: &Value) {
        let cancelled = self.receive().await;
        assert_eq!(
            cancelled["method"], "notifications/cancelled",
            "{cancelled}"
        );
        assert_eq!(cancelled["params"]["requestId"], request["id"]);
    }
}

fn collecting() -> (RecordSink, Arc<Mutex<Vec<Value>>>) {
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let sink = accepted.clone();
    let records = RecordSink::new(move |record| {
        sink.lock().unwrap().push(record);
        async { Ok(()) }
    });
    (records, accepted)
}

#[tokio::test]
async fn a_streaming_tool_passes_records_to_the_sink_and_acknowledges_each_after_it() {
    let (connection, mut remote) = Remote::connect().await;
    let binding = remote.bind(&connection).await;
    assert!(binding.operations()[0].streams);
    let (records, accepted) = collecting();
    let mut call = call("remote.export", json!({}));
    call.records = records;
    let executor = binding.executor();
    let calling = tokio::spawn(async move { executor.call(call).await });
    let (request, token) = remote.call().await;
    for sequence in [1, 2] {
        remote.send(record(&token, sequence)).await;
        let acknowledgement = remote.acknowledgement(&token, sequence).await;
        assert_eq!(accepted.lock().unwrap().len(), sequence as usize);
        remote.reply(&acknowledgement, json!({})).await;
    }
    remote
        .reply(&request, json!({"content":[],"structuredContent":2}))
        .await;
    let outcome = calling.await.unwrap().unwrap();
    assert_eq!(outcome.result, Some(json!(2)));
    assert_eq!(
        *accepted.lock().unwrap(),
        vec![json!({"item":1}), json!({"item":2})]
    );
}

#[tokio::test]
async fn a_result_completes_the_call_while_its_acknowledgement_reply_is_outstanding() {
    let (connection, mut remote) = Remote::connect().await;
    let executor = remote.bind(&connection).await.executor();
    let (records, accepted) = collecting();
    let mut call = call("remote.export", json!({}));
    call.records = records;
    let calling = tokio::spawn(async move { executor.call(call).await });
    let (request, token) = remote.call().await;
    remote.send(record(&token, 1)).await;
    remote.acknowledgement(&token, 1).await;
    // The acknowledgement released the result; its own reply never arrives.
    remote
        .reply(&request, json!({"content":[],"structuredContent":1}))
        .await;
    let outcome = calling.await.unwrap().unwrap();
    assert_eq!(outcome.result, Some(json!(1)));
    assert_eq!(*accepted.lock().unwrap(), vec![json!({"item":1})]);
}

#[tokio::test]
async fn a_refused_acknowledgement_cancels_the_call_with_uncertain_evidence() {
    let (connection, mut remote) = Remote::connect().await;
    let executor = remote.bind(&connection).await.executor();
    let (records, _) = collecting();
    let mut call = call("remote.export", json!({}));
    call.records = records;
    let calling = tokio::spawn(async move { executor.call(call).await });
    let (request, token) = remote.call().await;
    remote.send(record(&token, 1)).await;
    let acknowledgement = remote.acknowledgement(&token, 1).await;
    remote
        .send(json!({"jsonrpc":"2.0","id":acknowledgement["id"],
            "error":{"code":-32602,"message":"the record acknowledgement is unknown"}}))
        .await;
    remote.cancelled(&request).await;
    let refusal = calling.await.unwrap().unwrap_err();
    assert_eq!(refusal.code, "mcp_invalid_input");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
}

#[tokio::test]
async fn a_sink_refusal_cancels_the_call_without_acknowledging() {
    let (connection, mut remote) = Remote::connect().await;
    let executor = remote.bind(&connection).await.executor();
    let mut call = call("remote.export", json!({}));
    call.records = RecordSink::new(|_| async {
        Err(
            Refusal::new("output_failed", Category::Unavailable, "the sink closed")
                .with_evidence(Evidence::Uncertain),
        )
    });
    let calling = tokio::spawn(async move { executor.call(call).await });
    let (request, token) = remote.call().await;
    remote.send(record(&token, 1)).await;
    remote.cancelled(&request).await;
    let refusal = calling.await.unwrap().unwrap_err();
    assert_eq!(refusal.evidence, Evidence::Uncertain);
}

#[tokio::test]
async fn a_record_out_of_sequence_cancels_the_call() {
    let (connection, mut remote) = Remote::connect().await;
    let executor = remote.bind(&connection).await.executor();
    let (records, accepted) = collecting();
    let mut call = call("remote.export", json!({}));
    call.records = records;
    let calling = tokio::spawn(async move { executor.call(call).await });
    let (request, token) = remote.call().await;
    remote.send(record(&token, 2)).await;
    remote.cancelled(&request).await;
    let refusal = calling.await.unwrap().unwrap_err();
    assert_eq!(refusal.code, "mcp_protocol");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert!(accepted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn an_abandoned_catalogue_read_closes_the_connection() {
    let (connection, mut remote) = Remote::connect().await;
    let reading = tokio::spawn({
        let connection = connection.clone();
        async move { connection.operations().await }
    });
    let request = remote.receive().await;
    assert_eq!(
        request["method"],
        "io.github.thirtytwobits.clapham/catalogue"
    );
    reading.abort();
    let _ = reading.await;
    // The connection closes rather than keep the abandoned request's state.
    assert!(remote.lines.next_line().await.unwrap().is_none());
    assert!(connection.operations().await.is_err());
}

/// A connection that admits one lifetime notification.
fn sparing() -> McpConfig {
    let mut config = McpConfig::new("remote", "scope");
    config.bounds.notifications = 1;
    config
}

#[tokio::test]
async fn a_direct_call_streams_records_without_spending_the_notification_budget() {
    let (connection, mut remote) = Remote::connect_with(sparing()).await;
    let (records, accepted) = collecting();
    let meta = json!({"io.github.thirtytwobits.clapham/preview":true});
    let calling = tokio::spawn(async move {
        connection
            .call_tool(
                "export",
                serde_json::Map::new(),
                meta.as_object().unwrap().clone(),
                records,
            )
            .await
    });
    let (request, token) = remote.call().await;
    assert_eq!(request["params"]["name"], "export");
    assert_eq!(
        request["params"]["_meta"]["io.github.thirtytwobits.clapham/preview"],
        true
    );
    // Acknowledgement paces each record, so none counts against the lifetime budget.
    for sequence in 1..=5 {
        remote.send(record(&token, sequence)).await;
        let acknowledgement = remote.acknowledgement(&token, sequence).await;
        remote.reply(&acknowledgement, json!({})).await;
    }
    remote
        .reply(&request, json!({"content":[],"structuredContent":5}))
        .await;
    assert_eq!(calling.await.unwrap().unwrap().result, Some(json!(5)));
    assert_eq!(accepted.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn records_sent_ahead_of_their_acknowledgement_spend_the_notification_budget() {
    let (connection, mut remote) = Remote::connect_with(sparing()).await;
    let (records, _) = collecting();
    let calling = tokio::spawn(async move {
        connection
            .call_tool(
                "export",
                serde_json::Map::new(),
                serde_json::Map::new(),
                records,
            )
            .await
    });
    let (_, token) = remote.call().await;
    // The first is paced; the second and third arrive while it is held.
    for sequence in 1..=3 {
        remote.send(record(&token, sequence)).await;
    }
    let refusal = calling.await.unwrap().unwrap_err();
    assert_eq!(refusal.code, "mcp_limit");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
}

/// Answers the first acknowledgement as if its reply were lost, then accepts the repeat.
#[derive(Default)]
struct Lossy {
    events: Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>,
    call: Mutex<Option<Value>>,
    acknowledgements: Mutex<Vec<Value>>,
    attempts: AtomicUsize,
}

fn event(message: Value) -> String {
    format!("data: {message}\n\n")
}

async fn lossy(State(server): State<Arc<Lossy>>, Json(request): Json<Value>) -> Response {
    let reply = |result: Value| Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
    match request["method"].as_str().unwrap() {
        "server/discover" => reply(discovered()).into_response(),
        "tools/list" => reply(tools()).into_response(),
        "tools/call" => {
            let token = request["params"]["_meta"][RECORDS].as_str().unwrap();
            let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
            events.send(event(record(token, 1))).unwrap();
            *server.events.lock().unwrap() = Some(events);
            *server.call.lock().unwrap() = Some(request["id"].clone());
            let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
                let event = receiver.recv().await?;
                Some((Ok::<_, std::convert::Infallible>(event), receiver))
            });
            (
                [("content-type", "text/event-stream")],
                axum::body::Body::from_stream(body),
            )
                .into_response()
        }
        ACKNOWLEDGE => {
            server
                .acknowledgements
                .lock()
                .unwrap()
                .push(request["params"].clone());
            if server.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let id = server.call.lock().unwrap().clone().unwrap();
            let events = server.events.lock().unwrap().take().unwrap();
            events
                .send(event(json!({"jsonrpc":"2.0","id":id,
                    "result":{"content":[],"structuredContent":1}})))
                .unwrap();
            reply(json!({})).into_response()
        }
        _ => StatusCode::ACCEPTED.into_response(),
    }
}

#[tokio::test]
async fn a_lost_acknowledgement_reply_is_repeated_over_http() {
    let server = Arc::new(Lossy::default());
    let app = Router::new()
        .route("/mcp", post(lossy))
        .with_state(server.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let task = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let connection = McpConnection::http(
        McpHttpOptions::new(endpoint),
        McpConfig::new("remote", "scope"),
    )
    .await
    .unwrap();
    let binding = connection
        .discover()
        .await
        .unwrap()
        .bind(vec![McpSelection::new("export")])
        .unwrap();
    let (records, accepted) = collecting();
    let mut call = call("remote.export", json!({}));
    call.records = records;
    let outcome = binding.executor().call(call).await.unwrap();
    assert_eq!(outcome.result, Some(json!(1)));
    assert_eq!(*accepted.lock().unwrap(), vec![json!({"item":1})]);
    let acknowledgements = server.acknowledgements.lock().unwrap().clone();
    assert_eq!(acknowledgements.len(), 2);
    for acknowledgement in acknowledgements {
        assert_eq!(acknowledgement["sequence"], 1);
    }
    task.abort();
}
