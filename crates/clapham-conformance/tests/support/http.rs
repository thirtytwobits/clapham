//! Shared scripted HTTP authentication and JSON request construction.

use bytes::Bytes;
use clapham::OAuthContext;
use clapham_mcp_server::{Credential, HttpServer, PREVIEW_META_KEY};
use http::{Request, StatusCode};
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};

pub fn authenticate(credential: &Credential) -> Option<OAuthContext> {
    let scopes: &[&str] = match credential.authorization()? {
        "Bearer human" => &["lists:read", "lists:write", "lists:admin", "items:tick"][..],
        "Bearer agent" => &["lists:read", "lists:write"][..],
        _ => return None,
    };
    Some(oauth_context("credential-owner", scopes))
}

pub fn metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{},
        "io.modelcontextprotocol/clientInfo":{"name":"lists-client","version":"1"},
        PREVIEW_META_KEY:false,
    })
}

pub fn request(method: &str, mut params: Value, credential: &str) -> Request<Full<Bytes>> {
    let mut meta = metadata();
    if let Some(provided) = params.get("_meta").and_then(Value::as_object) {
        meta.as_object_mut().unwrap().extend(provided.clone());
    }
    params["_meta"] = meta;
    let mut builder = Request::post("/mcp")
        .header("host", "localhost")
        .header("authorization", credential)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", method);
    if let Some(name) = params
        .get("name")
        .or_else(|| params.get("uri"))
        .and_then(Value::as_str)
    {
        builder = builder.header("mcp-name", name);
    }
    builder
        .body(Full::new(Bytes::from(
            json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
        )))
        .unwrap()
}

pub async fn result(server: &HttpServer, method: &str, params: Value, credential: &str) -> Value {
    let response = server.handle(request(method, params, credential)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("mcp-session-id"));
    let value: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(value.get("error").is_none(), "{value}");
    value["result"].clone()
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}

#[allow(dead_code)]
pub fn compose(
    contributions: impl IntoIterator<Item = clapham::Contribution>,
    bounds: clapham::Bounds,
) -> Result<clapham::ComposedSet, clapham::CompositionError> {
    compose_with_options(contributions, bounds, clapham::ComposeOptions::default())
}
#[allow(dead_code)]
pub fn compose_with_options(
    contributions: impl IntoIterator<Item = clapham::Contribution>,
    bounds: clapham::Bounds,
    mut options: clapham::ComposeOptions,
) -> Result<clapham::ComposedSet, clapham::CompositionError> {
    options.oauth = Some(clapham::OAuthResource {
        issuer: "https://identity.example.com".into(),
        audience: "https://lists.example.com".into(),
    });
    clapham::compose_with_options(contributions, bounds, options)
}
