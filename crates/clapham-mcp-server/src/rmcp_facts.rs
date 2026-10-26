//! Evidence for the rmcp 3.6.0 server behaviour recorded in `docs/mcp.md`, through the
//! stdio transport and stateless HTTP configuration that Clapham serves.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use clapham::Bounds;
use http::{Request, StatusCode, header};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    model::*,
    service::{NotificationContext, RequestContext, RoleServer, RunningService},
    transport::streamable_http_server::{
        StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf},
    sync::{mpsc, oneshot},
};

use crate::{http::stateless_config, stdio::BoundedTransport};

const RECORD: &str = "example.test/record";
const ACKNOWLEDGE: &str = "example.test/acknowledge";
const TEMPLATE: &str = "example://{list}/{id}{?text,labels*}";

/// The `held` tool sends two custom notifications, then waits until the test releases it.
#[derive(Clone)]
struct Scripted {
    release: Arc<Mutex<Option<oneshot::Receiver<()>>>>,
    notified: mpsc::UnboundedSender<String>,
}

fn scripted() -> (
    Scripted,
    oneshot::Sender<()>,
    mpsc::UnboundedReceiver<String>,
) {
    let (release, held) = oneshot::channel();
    let (notified, observed) = mpsc::unbounded_channel();
    let handler = Scripted {
        release: Arc::new(Mutex::new(Some(held))),
        notified,
    };
    (handler, release, observed)
}

impl ServerHandler for Scripted {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Owned(vec![ProtocolVersion::V_2026_07_28])
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if request.name == "held" {
            for sequence in [1, 2] {
                context
                    .peer
                    .send_notification(ServerNotification::CustomNotification(
                        CustomNotification::new(RECORD, Some(json!({"sequence":sequence}))),
                    ))
                    .await
                    .map_err(|_| ErrorData::internal_error("notification not sent", None))?;
            }
            let held = self.release.lock().unwrap().take().expect("one held call");
            held.await.expect("the test releases the held call");
        }
        Ok(CallToolResult::structured(json!({"done":true})).into())
    }

    async fn list_resource_templates(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(vec![
            ResourceTemplate::new(TEMPLATE, "probe"),
        ]))
    }

    /// The contents echo the URI the handler received.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            request.uri.clone(),
            request.uri,
        )])
        .into())
    }

    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _: RequestContext<RoleServer>,
    ) -> Result<CustomResult, ErrorData> {
        Ok(CustomResult::new(json!({"method":request.method})))
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleServer>,
    ) {
        let _ = self.notified.send(notification.method);
    }
}

fn message(id: Option<u64>, method: &str, mut params: Value) -> Value {
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{},
        "io.modelcontextprotocol/clientInfo":{"name":"facts","version":"1"},
    });
    let mut message = json!({"jsonrpc":"2.0","method":method,"params":params});
    if let Some(id) = id {
        message["id"] = json!(id);
    }
    message
}

fn assert_record(message: &Value, sequence: u64) {
    assert_eq!(message["method"], RECORD, "{message}");
    assert_eq!(message["params"]["sequence"], sequence, "{message}");
    assert!(message.get("id").is_none(), "{message}");
}

struct Stdio {
    running: RunningService<RoleServer, Scripted>,
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
}

impl Stdio {
    /// rmcp selects the connection lifecycle from the first request, so serving
    /// completes only after that request is written.
    async fn start(handler: Scripted, first: Value) -> Self {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(server);
        let serving =
            tokio::spawn(handler.serve(BoundedTransport::new(read, write, Bounds::default())));
        let (read, mut write) = tokio::io::split(client);
        send(&mut write, first).await;
        Self {
            running: serving.await.unwrap().unwrap(),
            lines: BufReader::new(read).lines(),
            write,
        }
    }

    async fn send(&mut self, message: Value) {
        send(&mut self.write, message).await;
    }

    async fn receive(&mut self) -> Value {
        let line = self.lines.next_line().await.unwrap().expect("server open");
        serde_json::from_str(&line).unwrap()
    }
}

async fn send(write: &mut WriteHalf<DuplexStream>, message: Value) {
    write
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}

#[tokio::test]
async fn held_call_notifications_precede_its_result_in_order_over_stdio() {
    let (handler, release, _) = scripted();
    let mut stdio = Stdio::start(
        handler,
        message(Some(1), "tools/call", json!({"name":"held"})),
    )
    .await;
    // The handler has not returned, so these lines cannot be part of its result.
    assert_record(&stdio.receive().await, 1);
    assert_record(&stdio.receive().await, 2);
    release.send(()).unwrap();
    let result = stdio.receive().await;
    assert_eq!(result["id"], 1);
    assert_eq!(result["result"]["structuredContent"]["done"], true);
    stdio.running.cancel().await.unwrap();
}

#[tokio::test]
async fn custom_requests_reach_the_handler_while_a_call_is_held_over_stdio() {
    let (handler, release, _) = scripted();
    let mut stdio = Stdio::start(
        handler,
        message(Some(1), "tools/call", json!({"name":"held"})),
    )
    .await;
    assert_record(&stdio.receive().await, 1);
    assert_record(&stdio.receive().await, 2);
    stdio.send(message(Some(2), ACKNOWLEDGE, json!({}))).await;
    let acknowledged = stdio.receive().await;
    assert_eq!(acknowledged["id"], 2);
    assert_eq!(acknowledged["result"]["method"], ACKNOWLEDGE);
    release.send(()).unwrap();
    assert_eq!(stdio.receive().await["id"], 1);
    stdio.running.cancel().await.unwrap();
}

#[tokio::test]
async fn resource_templates_and_read_uris_pass_through_unchanged_over_stdio() {
    let (handler, _, _) = scripted();
    let mut stdio = Stdio::start(
        handler,
        message(Some(1), "resources/templates/list", json!({})),
    )
    .await;
    let listed = stdio.receive().await;
    assert_eq!(
        listed["result"]["resourceTemplates"][0]["uriTemplate"], TEMPLATE,
        "{listed}"
    );
    // Neither expanded from the template nor valid for it: rmcp does not interpret the URI.
    let uri = "example://a/b%20c?text=&labels=%2F&+?#";
    stdio
        .send(message(Some(2), "resources/read", json!({"uri":uri})))
        .await;
    let read = stdio.receive().await;
    assert_eq!(read["result"]["contents"][0]["text"], uri, "{read}");
    stdio.running.cancel().await.unwrap();
}

fn http_service(handler: Scripted) -> StreamableHttpService<Scripted, LocalSessionManager> {
    StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        stateless_config(Bounds::default().message_bytes),
    )
}

fn post(message: &Value) -> Request<Full<Bytes>> {
    let mut builder = Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", message["method"].as_str().unwrap());
    if let Some(name) = message["params"]["name"].as_str() {
        builder = builder.header("mcp-name", name);
    }
    builder
        .body(Full::new(Bytes::from(message.to_string())))
        .unwrap()
}

fn content_type(response: &http::Response<BoxBody<Bytes, std::convert::Infallible>>) -> &str {
    response.headers()[header::CONTENT_TYPE].to_str().unwrap()
}

/// JSON-RPC messages from the `data` fields of an SSE body, read as the server writes them.
struct Events {
    body: BoxBody<Bytes, std::convert::Infallible>,
    buffer: String,
}

impl Events {
    async fn next(&mut self) -> Option<Value> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..end + 2).collect();
                let data: Vec<&str> = event
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .map(|data| data.strip_prefix(' ').unwrap_or(data))
                    .collect();
                if data.is_empty() {
                    continue;
                }
                return Some(serde_json::from_str(&data.join("\n")).unwrap());
            }
            let frame = self.body.frame().await?.unwrap();
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }
}

#[tokio::test]
async fn stateless_http_answers_a_call_without_notifications_as_json() {
    let (handler, _, _) = scripted();
    let response = http_service(handler)
        .handle(post(&message(
            Some(1),
            "tools/call",
            json!({"name":"plain"}),
        )))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), "application/json");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(result["result"]["structuredContent"]["done"], true);
}

#[tokio::test]
async fn held_call_notifications_precede_its_result_in_order_over_stateless_http() {
    let (handler, release, _) = scripted();
    let response = http_service(handler)
        .handle(post(&message(
            Some(1),
            "tools/call",
            json!({"name":"held"}),
        )))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(content_type(&response), "text/event-stream");
    let mut events = Events {
        body: response.into_body(),
        buffer: String::new(),
    };
    // The handler has not returned, so these events cannot be part of its result.
    assert_record(&events.next().await.unwrap(), 1);
    assert_record(&events.next().await.unwrap(), 2);
    release.send(()).unwrap();
    let result = events.next().await.unwrap();
    assert_eq!(result["id"], 1);
    assert_eq!(result["result"]["structuredContent"]["done"], true);
    assert!(events.next().await.is_none());
}

#[tokio::test]
async fn custom_requests_reach_the_handler_while_a_call_is_held_over_stateless_http() {
    let (handler, release, _) = scripted();
    let service = http_service(handler);
    let held = service
        .handle(post(&message(
            Some(1),
            "tools/call",
            json!({"name":"held"}),
        )))
        .await;
    let mut events = Events {
        body: held.into_body(),
        buffer: String::new(),
    };
    assert_record(&events.next().await.unwrap(), 1);
    let response = service
        .handle(post(&message(Some(2), ACKNOWLEDGE, json!({}))))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let acknowledged: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(acknowledged["id"], 2);
    assert_eq!(acknowledged["result"]["method"], ACKNOWLEDGE);
    release.send(()).unwrap();
    assert_record(&events.next().await.unwrap(), 2);
    assert_eq!(events.next().await.unwrap()["id"], 1);
}

#[tokio::test]
async fn stateless_http_accepts_client_notifications_without_calling_the_handler() {
    let (handler, _, mut notified) = scripted();
    let service = http_service(handler);
    let response = service
        .handle(post(&message(None, ACKNOWLEDGE, json!({}))))
        .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    // A completed request on the same service shows the handler is reachable.
    let response = service
        .handle(post(&message(Some(1), ACKNOWLEDGE, json!({}))))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(notified.try_recv().is_err());
}
