//! Generate and check the lists contribution's MCP wire-form record.

#[path = "support/http.rs"]
mod support;

use std::{fs, path::PathBuf, sync::Arc};

use clapham::{Bounds, ComposeOptions, compose_with_options};
use clapham_conformance::{ListsGate, ListsHandle};
use clapham_mcp_server::HttpServer;
use serde_json::{Value, json};

#[tokio::test]
async fn mcp_protocol_record_is_current() {
    let lists = ListsHandle::new();
    let set = compose_with_options(
        lists.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let server = HttpServer::new(set, support::authenticate).unwrap();
    let tools = support::result(&server, "tools/list", json!({}), "Bearer human").await;
    let resources = support::result(
        &server,
        "resources/templates/list",
        json!({}),
        "Bearer human",
    )
    .await;
    let discovery = support::result(&server, "server/discover", json!({}), "Bearer human").await;
    let mut meta_keys = support::metadata()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    meta_keys.sort();
    let record = json!({
        "protocolVersions":discovery["supportedVersions"],
        "tools":tools["tools"],
        "resourceTemplates":resources["resourceTemplates"],
        "requestMetaKeys":meta_keys,
    });
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("records/mcp-protocol.json");
    if std::env::var_os("CLAPHAM_WRITE_RECORDS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("{}\n", serde_json::to_string_pretty(&record).unwrap()),
        )
        .unwrap();
    }
    let recorded: Value = serde_json::from_slice(&fs::read(&path).expect("generate the wire record with CLAPHAM_WRITE_RECORDS=1 cargo test -p clapham-conformance --test mcp_protocol")).unwrap();
    assert_eq!(
        recorded, record,
        "stale mcp-protocol.json; review the change, then regenerate with CLAPHAM_WRITE_RECORDS=1 cargo test -p clapham-conformance --test mcp_protocol"
    );
}
