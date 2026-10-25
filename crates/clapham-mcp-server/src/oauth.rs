//! OAuth resource-server verification using RFC 7662 introspection.

use clapham::{OAuthContext, OAuthResource};
use oauth2::{
    AccessToken, ClientId, ClientSecret, IntrospectionUrl, TokenIntrospectionResponse,
    basic::{BasicErrorResponse, BasicRevocationErrorResponse, BasicTokenResponse, BasicTokenType},
};

use crate::{Authenticator, Credential};

/// Introspect bearer tokens with a configured authorisation server.
///
/// The authorisation server controls grant issuance, revocation and scope
/// elevation. The resource server requires matching issuer/audience and checks
/// active status, expiry and not-before claims on each request. Redirects are
/// disabled so credentials cannot be forwarded to another endpoint.
/// The introspection response must include matching `iss` and `aud` claims.
/// `exp` and `nbf` are checked when provided; otherwise the authorisation server
/// establishes validity through its `active` response.
#[derive(Clone)]
pub struct OAuthIntrospector {
    resource: OAuthResource,
    endpoint: IntrospectionUrl,
    client_id: ClientId,
    secret: ClientSecret,
    http: reqwest::Client,
    clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl std::fmt::Debug for OAuthIntrospector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthIntrospector")
            .field("resource", &self.resource)
            .finish_non_exhaustive()
    }
}

impl OAuthIntrospector {
    /// Configure a confidential resource server's introspection credentials.
    /// HTTPS is required except for explicit loopback development endpoints.
    pub fn new(
        resource: OAuthResource,
        endpoint: impl Into<String>,
        client_id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let endpoint = IntrospectionUrl::new(endpoint.into())?;
        let loopback = matches!(
            endpoint.url().host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if endpoint.url().scheme() != "https" && !(loopback && endpoint.url().scheme() == "http") {
            return Err("OAuth introspection requires HTTPS".into());
        }
        if !endpoint.url().username().is_empty()
            || endpoint.url().password().is_some()
            || endpoint.url().fragment().is_some()
        {
            return Err("invalid introspection endpoint".into());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self {
            resource,
            endpoint,
            client_id: ClientId::new(client_id.into()),
            secret: ClientSecret::new(secret.into()),
            http,
            clock: std::sync::Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system clock is after the epoch")
                    .as_secs()
                    .try_into()
                    .unwrap_or(i64::MAX)
            }),
        })
    }

    /// Supply an explicit verification clock, including deterministic test clocks.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = std::sync::Arc::new(clock);
        self
    }

    async fn verify(&self, credential: &Credential) -> Option<OAuthContext> {
        let value = credential.authorization()?;
        if value.len() > 65536 {
            return None;
        }
        let (scheme, token) = value.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("Bearer")
            || token.is_empty()
            || token.chars().any(char::is_whitespace)
        {
            return None;
        }
        let client = oauth2::Client::<
            BasicErrorResponse,
            BasicTokenResponse,
            GrantResponse,
            oauth2::StandardRevocableToken,
            BasicRevocationErrorResponse,
        >::new(self.client_id.clone())
        .set_client_secret(self.secret.clone())
        .set_introspection_url(self.endpoint.clone());
        let token = AccessToken::new(token.to_owned());
        let response = client
            .introspect(&token)
            .request_async(&IntrospectionHttp(&self.http))
            .await
            .ok()?;
        let now = (self.clock)();
        authority(&self.resource, &response, now)
    }
}

impl Authenticator for OAuthIntrospector {
    fn authenticate<'a>(
        &'a self,
        credential: &'a Credential,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<OAuthContext>> + Send + 'a>> {
        Box::pin(self.verify(credential))
    }
}

fn authority(resource: &OAuthResource, response: &GrantResponse, now: i64) -> Option<OAuthContext> {
    if !response.active()
        || response.iss() != Some(resource.issuer.as_str())
        || !response
            .aud()
            .is_some_and(|audiences| audiences.contains(&resource.audience))
        || response
            .exp()
            .is_some_and(|expiry| expiry.timestamp() <= now)
        || response.nbf().is_some_and(|start| start.timestamp() > now)
    {
        return None;
    }
    let subject = response.sub().filter(|subject| !subject.is_empty())?;
    let mut context = OAuthContext::from_verified(
        &resource.issuer,
        &resource.audience,
        subject,
        response
            .scopes()
            .into_iter()
            .flatten()
            .map(|scope| scope.as_str()),
    );
    if let Some(client) = response.client_id() {
        context = context.with_client(client.as_str());
    }
    for (name, value) in &response.extra_fields().claims {
        context = context.with_claim(name, value.clone());
    }
    Some(context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn introspected_authority_checks_activity_issuer_audience_and_validity() {
        let resource = OAuthResource {
            issuer: "https://identity.example.com".into(),
            audience: "https://lists.example.com".into(),
        };
        let valid = json!({"active":true,"iss":resource.issuer,"aud":[resource.audience],"sub":"alice","client_id":"assistant","scope":"lists:read items:tick","exp":200,"nbf":50});
        let access = authority(
            &resource,
            &serde_json::from_value(valid.clone()).unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(access.subject(), "alice");
        assert!(access.has_scope("items:tick"));
        assert!(!access.has_scope("lists:delete"));
        let mut no_expiry = valid.clone();
        no_expiry.as_object_mut().unwrap().remove("exp");
        assert!(authority(&resource, &serde_json::from_value(no_expiry).unwrap(), 100).is_some());
        for claim in ["iss", "aud"] {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(claim);
            assert!(authority(&resource, &serde_json::from_value(missing).unwrap(), 100).is_none());
        }
        for (key, value) in [
            ("active", json!(false)),
            ("iss", json!("https://other.example.com")),
            ("aud", json!(["other"])),
            ("sub", json!("")),
            ("exp", json!(100)),
            ("nbf", json!(101)),
        ] {
            let mut invalid = valid.clone();
            invalid[key] = value;
            assert!(authority(&resource, &serde_json::from_value(invalid).unwrap(), 100).is_none());
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct GrantFields {
    #[serde(flatten)]
    claims: std::collections::BTreeMap<String, serde_json::Value>,
}
impl oauth2::ExtraTokenFields for GrantFields {}
type GrantResponse = oauth2::StandardTokenIntrospectionResponse<GrantFields, BasicTokenType>;

// The OAuth library owns request/response semantics; the transport bounds bytes
// before handing an introspection document to its deserialiser.
struct IntrospectionHttp<'a>(&'a reqwest::Client);
impl<'c> oauth2::AsyncHttpClient<'c> for IntrospectionHttp<'_> {
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + Sync + 'c>,
    >;
    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let mut response = self
                .0
                .execute(request.try_into().map_err(std::io::Error::other)?)
                .await
                .map_err(std::io::Error::other)?;
            let mut builder = http::Response::builder().status(response.status());
            *builder.headers_mut().expect("response headers") = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(std::io::Error::other)? {
                if bytes.len().saturating_add(chunk.len()) > 65536 {
                    return Err(std::io::Error::other(
                        "OAuth introspection response exceeds 64 KiB",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            builder.body(bytes).map_err(std::io::Error::other)
        })
    }
}
