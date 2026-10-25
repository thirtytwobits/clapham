use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// OAuth resource-server configuration. Token verification belongs to the adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OAuthResource {
    /// Trusted authorisation-server issuer.
    pub issuer: String,
    /// Required token audience.
    pub audience: String,
}

/// Authority established by trusted OAuth token verification or introspection.
///
/// Construct only after verifying the token. Request arguments and metadata must
/// never be deserialised into this context. Stdio hosts bind verified authority
/// to their privately launched connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthContext {
    issuer: String,
    audience: String,
    subject: String,
    client_id: Option<String>,
    scopes: BTreeSet<String>,
    claims: std::collections::BTreeMap<String, serde_json::Value>,
}

impl OAuthContext {
    /// Bind claims verified by the host's OAuth middleware to this request.
    pub fn from_verified<I, S>(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        subject: impl Into<String>,
        scopes: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            subject: subject.into(),
            client_id: None,
            scopes: scopes.into_iter().map(Into::into).collect(),
            claims: Default::default(),
        }
    }
    /// Record the client identified by the verified grant.
    pub fn with_client(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }
    /// Attach an extension claim obtained from trusted token verification.
    pub fn with_claim(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.claims.insert(name.into(), value);
        self
    }
    /// A verified extension claim, such as a grant-specific resource restriction.
    pub fn claim(&self, name: &str) -> Option<&serde_json::Value> {
        self.claims.get(name)
    }
    /// The verified issuing authority.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    /// The verified audience.
    pub fn audience(&self) -> &str {
        &self.audience
    }
    /// The grant's subject within its issuer.
    pub fn subject(&self) -> &str {
        &self.subject
    }
    /// The client identified by the grant, where supplied.
    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref()
    }
    /// Granted OAuth scopes, in stable order.
    pub fn scopes(&self) -> &BTreeSet<String> {
        &self.scopes
    }
    /// Whether this grant contains a required scope.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains(scope)
    }
    /// Check the resource-server binding of this verified context.
    pub fn is_for(&self, resource: &OAuthResource) -> bool {
        self.issuer == resource.issuer
            && self.audience == resource.audience
            && !self.subject.is_empty()
    }
}
