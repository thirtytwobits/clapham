//! Generate and check the lists contribution's MCP wire-form record.

#[path = "support/http.rs"]
mod support;

use std::{fs, path::PathBuf, sync::Arc};

use clapham::{Bounds, ComposeOptions};
use clapham_conformance::{ListsGate, ListsHandle};
use clapham_mcp_server::{ACKNOWLEDGE_REQUEST, HttpServer, RECORDS_EXTENSION, REQUEST_META_KEYS};
use http_body_util::BodyExt;
use serde_json::{Value, json};

/// The wire form of one record notification and its acknowledgement, from a streaming call.
async fn records(server: &HttpServer) -> Value {
    let meta = json!({
        "io.modelcontextprotocol/clientCapabilities":{"extensions":{RECORDS_EXTENSION:{}}},
        RECORDS_EXTENSION:"record-token",
    });
    let response = server
        .handle(support::request(
            "tools/call",
            json!({"name":"lists.export","arguments":{"id":"tasks"},"_meta":meta}),
            "Bearer human",
        ))
        .await;
    let mut body = response.into_body();
    let mut buffer = Vec::new();
    let notification: Value = loop {
        if let Some(end) = buffer.windows(2).position(|pair| pair == b"\n\n") {
            let event = String::from_utf8(buffer[..end].to_vec()).unwrap();
            let data = event
                .lines()
                .find_map(|line| line.strip_prefix("data:"))
                .unwrap();
            break serde_json::from_str(data.trim_start()).unwrap();
        }
        let frame = body.frame().await.unwrap().unwrap();
        buffer.extend_from_slice(&frame.into_data().unwrap());
    };
    let acknowledgement = json!({"token":"record-token","sequence":1});
    let acknowledged = support::result(
        server,
        ACKNOWLEDGE_REQUEST,
        acknowledgement.clone(),
        "Bearer human",
    )
    .await;
    while body.frame().await.is_some() {}
    let keys = |value: &Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| *key != "_meta")
            .cloned()
            .collect::<Vec<_>>()
    };
    json!({
        "tokenMetaKey":RECORDS_EXTENSION,
        "notification":{"method":notification["method"],"params":keys(&notification["params"])},
        "acknowledgement":{"method":ACKNOWLEDGE_REQUEST,"params":keys(&acknowledgement),"result":acknowledged},
    })
}

#[tokio::test]
async fn mcp_protocol_record_is_current() {
    let lists = ListsHandle::new();
    lists.create_list("tasks", "Tasks").unwrap();
    lists.add_item("tasks", "first", "First").unwrap();
    let set = support::compose_with_options(
        lists.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate)
        .unwrap()
        .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
        .unwrap();
    let tools = support::result(&server, "tools/list", json!({}), "Bearer human").await;
    let resources = support::result(
        &server,
        "resources/templates/list",
        json!({}),
        "Bearer human",
    )
    .await;
    let discovery = support::result(&server, "server/discover", json!({}), "Bearer human").await;
    let record = json!({
        "protocolVersions":discovery["supportedVersions"],
        "extensions":discovery["capabilities"]["extensions"],
        "records":records(&server).await,
        "tools":tools["tools"],
        "resourceTemplates":resources["resourceTemplates"],
        "requestMetaKeys":REQUEST_META_KEYS,
    });
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("records/mcp-protocol.json");
    let mut record = record;
    canonicalise(&mut record);
    let expected = format!("{}\n", serde_json::to_string_pretty(&record).unwrap());
    if std::env::var_os("CLAPHAM_WRITE_RECORDS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &expected).unwrap();
    }
    let recorded = fs::read_to_string(&path).expect("generate the wire record with CLAPHAM_WRITE_RECORDS=1 cargo test -p clapham-conformance --test mcp_protocol");
    assert_eq!(
        recorded, expected,
        "stale or non-canonical mcp-protocol.json; review the change, then regenerate with CLAPHAM_WRITE_RECORDS=1 cargo test -p clapham-conformance --test mcp_protocol"
    );
}

fn canonicalise(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let mut fields = std::mem::take(object).into_iter().collect::<Vec<_>>();
            fields.sort_by(|(left, _), (right, _)| left.cmp(right));
            for (name, mut value) in fields {
                canonicalise(&mut value);
                object.insert(name, value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(canonicalise),
        _ => {}
    }
}
