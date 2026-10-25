# clapham-mcp-server

MCP adapters over the transport-independent operation runner. Requires Rust 1.88 and uses
rmcp 3.6.0. The `stdio` and `http` features can be selected independently.

`Server::new` binds verified OAuth authority supplied by the trusted stdio host.
`HttpServer::new` requires the set's OAuth resource binding and accepts an asynchronous
`Authenticator`; each request is authenticated
before its body or an operation is dispatched. `OAuthIntrospector` implements RFC 7662 token
verification with the OAuth library. It checks active status, issuer, audience, subject,
expiry and not-before. Verified extension claims remain available to resource policy.

```no_run
# #[cfg(feature = "http")]
# fn example() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
use clapham::{Bounds, Configuration};
use clapham_mcp_server::{HttpServer, OAuthIntrospector};

let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
    .compose(Bounds::default())?;
let verifier = OAuthIntrospector::new(
    set.oauth_resource().unwrap().clone(),
    "https://identity.example.com/introspect",
    std::env::var("CLAPHAM_OAUTH_CLIENT_ID")?,
    std::env::var("CLAPHAM_OAUTH_CLIENT_SECRET")?,
)?;
let router = HttpServer::new(set, verifier)?.into_router();
# let _ = router;
# Ok(())
# }
```

`into_router` mounts `/mcp` and the protected-resource metadata. The application owns its
listener, TLS and shutdown. `handle` allows explicit mounting. HTTP uses rmcp's Host/Origin policy and bounded
request execution. OAuth credentials and introspection secrets are omitted from debug output.
The authorisation server owns consent, token issuance, refresh and scope elevation.

Each MCP tool derives its name, description, schemas and hints from its operation. Native
rmcp content types are interpreted in the adapter. Tool errors carry unchanged structured
refusals. Successful effect evidence is carried in tool-result metadata. Resource reads and
discovery use the same scope and host-policy checks; calls additionally run resource policies.

MCP calls carry records under the records extension, each acknowledged after the client's sink
accepts it. A client without the extension receives the final result alone. Closing the
response stream cancels a call.

Resource templates, frame sizes, catalogue budgets, concurrency and output validation remain
bounded. Oversized existing refusals are not replaced with another domain code. See
[the adapter guide](../../docs/mcp.md) and [contract](../../docs/contract.md).

Register URI templates with `Server::resources` or `HttpServer::resources`. The adapter checks
query semantics, string variables and duplicate template shapes before serving.

