//! A real stdio server for the shared lists-and-items fixture.

use std::sync::Arc;

use clapham::{Bounds, ComposeOptions, Principal, PrincipalKind, compose_with_options};
use clapham_conformance::{ListsGate, ListsHandle};
use clapham_mcp_server::Server;

fn main() {
    let handle = ListsHandle::new();
    handle.create_list("tasks", "Tasks").unwrap();
    handle.create_list("a/b", "Encoded").unwrap();
    handle.add_item("tasks", "a", "Read").unwrap();
    let set = compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let kind = if std::env::args().any(|arg| arg == "agent") {
        PrincipalKind::Agent
    } else {
        PrincipalKind::Human
    };
    let server = Server::new(set, Principal::new("launcher", kind)).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            server.serve_stdio().await.unwrap().waiting().await.unwrap();
        });
}
