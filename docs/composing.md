# Composing an application

Define operations independently of their protocols. JSON and Rust builders produce the same
configuration model and use the same composition and validation path.

## Typed operations

This complete Rust example defines and composes a query:

```rust
use clapham::{
    Bounds, Call, Configuration, Contribution, Effect, Exposure, TypedOperation,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CountInput {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = TypedOperation::<CountInput, u64>::new(
        "lists.count",
        "Count the lists in this workspace.",
        Effect::Query,
    )?.require_scope("lists:read");
    let lists = Contribution::new("lists")
        .typed_operation(count.clone(), |_: Call, _: CountInput| async { Ok(2) });
    let set = Configuration::new(
        "https://identity.example.com",
        "https://lists.example.com",
    )
        .provider("lists", lists)
        .expose(Exposure::typed("lists", &count))
        .compose(Bounds::default())?;
    assert!(set.get("lists.count").is_some());
    Ok(())
}
```

The [typed executable](../examples/typed/src/main.rs) adds OAuth introspection, MCP/operation
HTTP and the generated CLI. Its server uses the configured issuer and audience above.
Supply `CLAPHAM_INTROSPECTION_URL`, `CLAPHAM_OAUTH_CLIENT_ID` and
`CLAPHAM_OAUTH_CLIENT_SECRET` for the authorisation server's introspection endpoint.
For the CLI, supply `CLAPHAM_ENDPOINT` and `CLAPHAM_ACCESS_TOKEN` from the application's
OAuth client or credential store. The token must target the configured audience and include
`lists:read`.

```console
cargo run -p clapham-typed -- serve
cargo run -p clapham-typed -- lists count
```

The application owns the listener, TLS, credentials, resource policy, workspace ownership,
persistence and shutdown. `OAuthIntrospector` uses the OAuth library for RFC 7662 requests,
then checks active status, issuer, audience, subject and validity. Grant issuance, consent
and scope elevation belong to the authorisation server.

## JSON and programmatic extension

The [lists configuration](../examples/lists/configuration.json) selects providers and
operations. The [executable](../examples/lists/src/main.rs) loads that JSON, registers the
providers and adds `items.tick` with a Rust builder before composition.

A permission binding has this form:

```json
{
  "source": "lists",
  "name": "lists.get",
  "require": {
    "scopes": ["lists:read"],
    "policy": "lists.can_read"
  }
}
```

Register `lists.can_read` with `ResourcePolicy::typed` over the operation's input type.
The policy checks ownership, sharing, account permissions and grant-specific restrictions
using the verified context. Its failure occurs before handler dispatch. JSON names a
registered implementation; Rust can register the implementation directly. All listed scopes
are required. Configuration adds restrictions to intrinsic provider requirements.

`Source::remote` names an endpoint and host credential reference. The host establishes its
OAuth-authenticated MCP connection, discovers/binds selected tools, then registers its
contribution with `register_connection`. Incoming tokens are not forwarded to upstream servers.

## Authorisation

The same subject may have multiple grants. An interactive grant can carry `lists:read`,
`items:tick` and `lists:delete`; an assistant grant can carry only the first two. The shared
runner checks scopes on every call. Resource policies additionally restrict which lists the
grant can access. Verified extension claims can carry grant-specific resource restrictions.

Effects describe behaviour and adapter hints. CLI confirmation does not grant authority.
Discovery filters operation-level permissions without evaluating resource policies that
require invocation input. The invocation must still pass resource authorisation.

See the [composition reference](composition.md), [call contract](calls.md),
[MCP adapters](mcp.md) and [conformance guide](conformance.md).
