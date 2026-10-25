#![doc = include_str!("../README.md")]

mod harness;
mod operations;
mod state;

pub use state::{ChangeKind, HeldCall, Item, List, ListChange, ListsGate, ListsHandle};

pub use harness::{
    Case, Failure, Fixture, Frame, FrontEnd, InProcess, Observation, ObservationFuture, Process,
    ProcessFrontEnd, ProcessKind, Report, lists_fixture, lists_gate, run,
};

/// Decode a trusted fixture host's scripted OAuth claim snapshot.
/// This fixture helper does not verify bearer tokens and must not authenticate
/// application traffic. Production adapters use OAuth token verification.
pub fn scripted_access(claims: &serde_json::Value) -> Option<clapham::OAuthContext> {
    let issuer = claims.get("issuer")?.as_str()?;
    let audience = claims.get("audience")?.as_str()?;
    let subject = claims.get("subject")?.as_str()?;
    let scopes = claims
        .get("scopes")?
        .as_array()?
        .iter()
        .map(|scope| scope.as_str())
        .collect::<Option<Vec<_>>>()?;
    let mut auth = clapham::OAuthContext::from_verified(issuer, audience, subject, scopes);
    if let Some(client) = claims.get("client_id").and_then(serde_json::Value::as_str) {
        auth = auth.with_client(client);
    }
    if let Some(claims) = claims.get("claims").and_then(serde_json::Value::as_object) {
        for (name, value) in claims {
            auth = auth.with_claim(name, value.clone());
        }
    }
    Some(auth)
}

/// MCP-only resource registrations for the lists fixture.
pub const LIST_RESOURCES: &[(&str, &str)] =
    &[("lists.list", "lists://"), ("lists.get", "lists://{id}")];
