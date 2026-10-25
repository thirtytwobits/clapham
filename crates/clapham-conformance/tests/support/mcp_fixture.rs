//! A real stdio server for the shared lists-and-items fixture.

use std::sync::Arc;

use clapham::{Bounds, ComposeOptions, compose_with_options};
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
        &["lists:read", "lists:write"][..]
    } else {
        &["lists:read", "lists:write", "lists:admin", "items:tick"][..]
    };
    let server = Server::new(set, oauth_context("launcher", kind))
        .unwrap()
        .resources(clapham_conformance::LIST_RESOURCES.iter().copied())
        .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            server.serve_stdio().await.unwrap().waiting().await.unwrap();
        });
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}
