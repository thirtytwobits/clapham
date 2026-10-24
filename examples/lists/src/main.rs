//! Reference application assembling the descriptor-derived CLI and both MCP servers.

use clapham::{Bounds, ComposeOptions, Principal, PrincipalKind, compose_with_options};
use clapham_cli::{Cli, process_principal};
use clapham_conformance::{lists_fixture, lists_gate};
use clapham_mcp_server::{Credential, HttpServer, Server};
use serde_json::json;
use std::{error::Error, io::Write, process::ExitCode};
use tokio::io::AsyncReadExt;

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(run());
    // Servers finish their work before returning. Abandon any cancelled console-stdin
    // read so it cannot hold runtime shutdown open after Ctrl-C or normal completion.
    runtime.shutdown_background();
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let refusal = clapham::Refusal::not_dispatched(
                "application_configuration",
                clapham::Category::Configuration,
                error.to_string(),
            );
            ExitCode::from(clapham_cli::write_refusal(&refusal, std::io::stderr()))
        }
    }
}
async fn run() -> Result<u8, Box<dyn Error>> {
    let set = compose_with_options(
        lists_fixture().contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: lists_gate(),
            ..Default::default()
        },
    )?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("serve") {
        return Ok(Cli::new("lists", set, process_principal()?)?.run().await);
    }
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", "--stdio"] => {
            Server::new(set, process_principal()?)?
                .serve_stdio()
                .await?
                .waiting()
                .await?;
        }
        ["serve", "--stdio", "--as", identity] => {
            Server::new(set, principal(identity)?)?
                .serve_stdio()
                .await?
                .waiting()
                .await?;
        }
        ["serve", "--http", address] => {
            let address: std::net::SocketAddr = address.parse()?;
            if !address.ip().is_loopback() {
                return Err(
                    "the example's scripted credentials require a loopback listener".into(),
                );
            }
            let server = HttpServer::new(set, authenticate)?;
            let app = axum::Router::new().route(
                "/mcp",
                axum::routing::any(move |request| {
                    let server = server.clone();
                    async move { server.handle(request).await }
                }),
            );
            let listener = tokio::net::TcpListener::bind(address).await?;
            // The bound address is the readiness handshake. No polling or guessed port is needed.
            println!("{}", json!({"address":listener.local_addr()?.to_string()}));
            std::io::stdout().flush()?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        _ => {
            return Err(
                "usage: lists serve --stdio [--as KIND:ID] | lists serve --http ADDR".into(),
            );
        }
    }
    Ok(0)
}
async fn shutdown() {
    let mut stdin = tokio::io::stdin();
    let mut byte = [0];
    tokio::select! {
        _ = stdin.read(&mut byte) => {},
        _ = tokio::signal::ctrl_c() => {},
    }
}
fn principal(identity: &str) -> Result<Principal, Box<dyn Error>> {
    let (kind, id) = identity.split_once(':').ok_or("expected KIND:ID")?;
    if id.is_empty() {
        return Err("principal identity is empty".into());
    }
    let kind = match kind {
        "human" => PrincipalKind::Human,
        "agent" => PrincipalKind::Agent,
        "service" => PrincipalKind::Service,
        "system" => PrincipalKind::System,
        _ => return Err("unknown principal kind".into()),
    };
    Ok(Principal::new(id, kind))
}
fn authenticate(credential: &Credential) -> Option<Principal> {
    let (id, kind) = match credential.authorization()? {
        "Bearer human:human" => ("human", PrincipalKind::Human),
        "Bearer agent:agent" => ("agent", PrincipalKind::Agent),
        "Bearer human:agent" => ("agent", PrincipalKind::Human),
        "Bearer agent:denied" => ("denied", PrincipalKind::Agent),
        _ => return None,
    };
    Some(Principal::new(id, kind))
}
