//! Reference application: mixed JSON/Rust composition and an authenticated CLI that calls
//! its daemon over MCP.

use clapham::{Bounds, ComposedSet, Configuration, Exposure, OAuthContext};
use clapham_cli::{Cli, McpBackend};
use clapham_conformance::{lists_fixture, lists_gate, scripted_access};
use clapham_mcp_server::{Authenticator, Credential, HttpServer, OAuthIntrospector, Server};
use serde_json::{Value, json};
use std::{
    error::Error,
    io::Write,
    process::{ExitCode, Stdio},
};
use tokio::io::AsyncReadExt;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn main() -> ExitCode {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(run());
    runtime.shutdown_background();
    match outcome {
        Ok(code) => ExitCode::from(code),
        Err(error) => ExitCode::from(clapham_cli::write_refusal(
            &clapham::Refusal::not_dispatched(
                "application_configuration",
                clapham::Category::Configuration,
                error.to_string(),
            ),
            std::io::stderr(),
        )),
    }
}

fn composed() -> Result<ComposedSet> {
    let mut contributions = lists_fixture().contributions().into_iter();
    let lists = contributions.next().expect("lists provider");
    let items = contributions.next().expect("items provider");
    Ok(
        Configuration::from_json(include_str!("../configuration.json"))?
            .register_provider("lists", lists)
            .register_provider("items", items)
            .expose(Exposure::new("items", "items.tick"))
            .compose_with_options(
                Bounds::default(),
                clapham::ComposeOptions {
                    gate: lists_gate(),
                    ..Default::default()
                },
            )?,
    )
}

fn fixture_mode() -> bool {
    std::env::var("CLAPHAM_FIXTURE_MODE").as_deref() == Ok("1")
}
fn fixture_authority() -> Result<OAuthContext> {
    let claims: Value = serde_json::from_str(&std::env::var("CLAPHAM_FIXTURE_ACCESS")?)?;
    scripted_access(&claims).ok_or_else(|| "invalid scripted fixture authority".into())
}
fn introspector(set: &ComposedSet) -> Result<OAuthIntrospector> {
    OAuthIntrospector::new(
        set.oauth_resource()
            .expect("configuration binds OAuth")
            .clone(),
        std::env::var("CLAPHAM_INTROSPECTION_URL")?,
        std::env::var("CLAPHAM_OAUTH_CLIENT_ID")?,
        std::env::var("CLAPHAM_OAUTH_CLIENT_SECRET")?,
    )
}
fn http_server(set: ComposedSet) -> Result<HttpServer> {
    if fixture_mode() {
        Ok(HttpServer::new(set, |credential: &Credential| {
            let claims = credential.authorization()?.strip_prefix("Bearer ")?;
            scripted_access(&serde_json::from_str::<Value>(claims).ok()?)
        })?
        .resources(clapham_conformance::LIST_RESOURCES.iter().copied())?)
    } else {
        let oauth = introspector(&set)?;
        Ok(HttpServer::new(set, oauth)?
            .resources(clapham_conformance::LIST_RESOURCES.iter().copied())?)
    }
}
fn routes(server: HttpServer) -> axum::Router {
    server.into_router()
}

async fn run() -> Result<u8> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("serve") {
        if std::env::var("CLAPHAM_TRANSPORT").as_deref() == Ok("stdio") {
            return cli_over_stdio().await;
        }
        if fixture_mode() {
            let access = fixture_authority()?;
            let server = http_server(composed()?)?;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = format!("http://{}", listener.local_addr()?);
            let stop = clapham::Cancellation::new();
            let stopped = stop.clone();
            let task = tokio::spawn(async move {
                axum::serve(listener, routes(server))
                    .with_graceful_shutdown(async move { stopped.cancelled().await })
                    .await
            });
            let backend =
                McpBackend::http(format!("{endpoint}/mcp"), serde_json::to_string(&access)?)
                    .await?;
            let result = Cli::connect("lists", backend, Bounds::default())
                .await?
                .run()
                .await;
            stop.cancel();
            task.await??;
            return Ok(result);
        }
        let backend = McpBackend::http(
            std::env::var("CLAPHAM_ENDPOINT")?,
            std::env::var("CLAPHAM_ACCESS_TOKEN")?,
        )
        .await?;
        return Ok(Cli::connect("lists", backend, Bounds::default())
            .await?
            .run()
            .await);
    }
    let set = composed()?;
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["serve", "--stdio"] => {
            let access = if fixture_mode() {
                fixture_authority()?
            } else {
                introspector(&set)?
                    .authenticate(&Credential::new(format!(
                        "Bearer {}",
                        std::env::var("CLAPHAM_ACCESS_TOKEN")?
                    )))
                    .await
                    .ok_or("the launch token was not verified")?
            };
            Server::new(set, access)?
                .resources(clapham_conformance::LIST_RESOURCES.iter().copied())?
                .serve_stdio()
                .await?
                .waiting()
                .await?;
        }
        ["serve", "--http", address] => {
            let listener = tokio::net::TcpListener::bind(address).await?;
            if fixture_mode() && !listener.local_addr()?.ip().is_loopback() {
                return Err("scripted fixture authentication requires loopback".into());
            }
            let app = routes(http_server(set)?);
            println!("{}", json!({"address":listener.local_addr()?.to_string()}));
            std::io::stdout().flush()?;
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown())
                .await?;
        }
        _ => return Err("expected serve --stdio or serve --http ADDR".into()),
    }
    Ok(0)
}
/// Launch this executable's stdio server, which verifies the launch credential in the
/// inherited environment, and run the CLI over its standard streams.
async fn cli_over_stdio() -> Result<u8> {
    let mut server = tokio::process::Command::new(std::env::current_exe()?)
        .args(["serve", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let input = server.stdin.take().ok_or("missing server stdin")?;
    let output = server.stdout.take().ok_or("missing server stdout")?;
    let backend = McpBackend::stdio(output, input).await?;
    // Dropping the CLI closes the connection and the server's stdin.
    let result = Cli::connect("lists", backend, Bounds::default())
        .await?
        .run()
        .await;
    if !server.wait().await?.success() {
        return Err("the stdio server failed".into());
    }
    Ok(result)
}
async fn shutdown() {
    let mut stdin = tokio::io::stdin();
    let mut byte = [0];
    tokio::select! { _ = stdin.read(&mut byte) => {}, _ = tokio::signal::ctrl_c() => {} }
}
