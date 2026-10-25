//! Typed operations, programmatic composition and an OAuth-authenticated generated CLI.

use clapham::{Bounds, Call, Configuration, Contribution, Effect, Exposure, TypedOperation};
use clapham_cli::{Cli, HttpBackend};
use clapham_mcp_server::{HttpServer, OAuthIntrospector};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{error::Error, process::ExitCode};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CountInput {}

fn configuration() -> Result<Configuration> {
    let count = TypedOperation::<CountInput, u64>::new(
        "lists.count",
        "Count the lists in this workspace.",
        Effect::Query,
    )?
    .require_scope("lists:read");
    let provider = Contribution::new("lists")
        .typed_operation(count.clone(), |_: Call, _: CountInput| async { Ok(2) });
    Ok(
        Configuration::new("https://identity.example.com", "https://lists.example.com")
            .provider("lists", provider)
            .expose(Exposure::typed("lists", &count)),
    )
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<ExitCode> {
    if std::env::args().nth(1).as_deref() == Some("serve") {
        let set = configuration()?.compose(Bounds::default())?;
        let verifier = OAuthIntrospector::new(
            set.oauth_resource()
                .expect("configured OAuth resource")
                .clone(),
            std::env::var("CLAPHAM_INTROSPECTION_URL")?,
            std::env::var("CLAPHAM_OAUTH_CLIENT_ID")?,
            std::env::var("CLAPHAM_OAUTH_CLIENT_SECRET")?,
        )?;
        let server = HttpServer::new(set, verifier)?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
        axum::serve(listener, server.into_router()).await?;
        Ok(ExitCode::SUCCESS)
    } else {
        let backend = HttpBackend::new(
            std::env::var("CLAPHAM_ENDPOINT")?,
            std::env::var("CLAPHAM_ACCESS_TOKEN")?,
            Bounds::default(),
        )?;
        let cli = Cli::connect("lists", backend, Bounds::default()).await?;
        Ok(ExitCode::from(cli.run().await))
    }
}
