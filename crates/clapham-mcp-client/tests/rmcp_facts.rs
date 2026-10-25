//! Evidence for the rmcp 3.6.0 client behaviour recorded in `docs/mcp.md`.

use std::sync::Mutex;

use rmcp::{
    ClientHandler,
    model::*,
    service::{
        ClientLifecycleMode, NotificationContext, RoleClient, RunningService,
        serve_client_with_lifecycle_and_ct,
    },
    transport::async_rw::AsyncRwTransport,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

const RECORD: &str = "example.test/record";

/// The scripted server end of a duplex connection, after the discovery handshake.
struct Remote {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
}

impl Remote {
    async fn connect<H: ClientHandler>(handler: H) -> (RunningService<RoleClient, H>, Self) {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(client);
        let starting = tokio::spawn(serve_client_with_lifecycle_and_ct(
            handler,
            AsyncRwTransport::new_client(read, write),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
            CancellationToken::new(),
        ));
        let (read, write) = tokio::io::split(server);
        let mut remote = Self {
            lines: BufReader::new(read).lines(),
            write,
        };
        let discover = remote.receive().await;
        assert_eq!(discover["method"], "server/discover");
        remote
            .send(json!({"jsonrpc":"2.0","id":discover["id"],"result":{
                "resultType":"complete","supportedVersions":["2026-07-28"],
                "capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private",
            }}))
            .await;
        (starting.await.unwrap().unwrap(), remote)
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
}

fn record(sequence: u64) -> Value {
    json!({"jsonrpc":"2.0","method":RECORD,"params":{"token":"call-1","sequence":sequence}})
}

struct Observer(mpsc::UnboundedSender<CustomNotification>);

impl ClientHandler for Observer {
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleClient>,
    ) {
        let _ = self.0.send(notification);
    }
}

#[tokio::test]
async fn custom_notifications_during_a_call_reach_the_client_handler() {
    let (observed, mut notifications) = mpsc::unbounded_channel();
    let (running, mut remote) = Remote::connect(Observer(observed)).await;
    let peer = running.peer().clone();
    let call = tokio::spawn(async move {
        peer.call_tool_once(CallToolRequestParams::new("held"))
            .await
    });
    let request = remote.receive().await;
    assert_eq!(request["method"], "tools/call");
    remote.send(record(1)).await;
    // The call is still in flight: its result has not been written.
    let notification = notifications.recv().await.unwrap();
    assert_eq!(notification.method, RECORD);
    assert_eq!(
        notification.params,
        Some(json!({"token":"call-1","sequence":1}))
    );
    remote
        .send(json!({"jsonrpc":"2.0","id":request["id"],"result":{
            "content":[],"structuredContent":{"done":true},
        }}))
        .await;
    call.await.unwrap().unwrap();
    running.cancel().await.unwrap();
}

/// Holds the first record's handler until the second record's handler has run.
struct Reordering {
    first: Mutex<Option<oneshot::Receiver<()>>>,
    second: Mutex<Option<oneshot::Sender<()>>>,
    observed: mpsc::UnboundedSender<u64>,
}

impl ClientHandler for Reordering {
    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _: NotificationContext<RoleClient>,
    ) {
        let sequence = notification.params.unwrap()["sequence"].as_u64().unwrap();
        if sequence == 1 {
            let held = self.first.lock().unwrap().take().unwrap();
            held.await.unwrap();
        } else {
            let release = self.second.lock().unwrap().take().unwrap();
            release.send(()).unwrap();
        }
        let _ = self.observed.send(sequence);
    }
}

#[tokio::test]
async fn custom_notification_handlers_are_not_ordered_with_one_another() {
    let (release, held) = oneshot::channel();
    let (observed, mut sequences) = mpsc::unbounded_channel();
    let handler = Reordering {
        first: Mutex::new(Some(held)),
        second: Mutex::new(Some(release)),
        observed,
    };
    let (running, mut remote) = Remote::connect(handler).await;
    remote.send(record(1)).await;
    remote.send(record(2)).await;
    // The first handler completes only because the second ran while it was held.
    assert_eq!(sequences.recv().await, Some(2));
    assert_eq!(sequences.recv().await, Some(1));
    running.cancel().await.unwrap();
}
