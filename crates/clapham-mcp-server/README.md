# clapham-mcp-server

Requires Rust 1.88. The `stdio` and `http` transport features are enabled by default and can
be selected independently. [`Server`] and `HttpServer` share the core call boundary; the
host supplies identity, transport ownership and any workspace lock.

`Server` exposes a composed set as MCP 2026-07-28 tools and resources, using rmcp 3.6.0.
The application supplies the principal when constructing the server; a stdio connection
keeps that identity for its lifetime. Request arguments and metadata cannot replace it.

```rust,no_run
use clapham::{ComposedSet, Principal};
use clapham_mcp_server::Server;

# #[cfg(feature = "stdio")]
# async fn example(set: ComposedSet, principal: Principal) -> Result<(), Box<dyn std::error::Error>> {
let server = Server::new(set, principal)?.select(["clapham.version"])?;
// The application owns the Tokio runtime and any workspace lock.
let running = server.serve_stdio().await?;
running.waiting().await?;
# Ok(())
# }
```

`select` takes complete operation names and limits discovery, calls and resources together.
The default selection includes every operation. Discovery uses named `clapham.describe`
calls for the connection's principal, including when that built-in is not exposed as a tool.
The gate must admit `clapham.describe` for that principal: denying it or leaving it pending
makes discovery and resource reads return a JSON-RPC internal error whose data carries the
original refusal. Hiding the describe tool with `select` does not deny its internal use.
Direct tool calls still use their own admission policy. Gate admission is checked again
at dispatch through `ComposedSet::call`. Resource discovery describes only operations that
have a resource; it does not validate unrelated tool descriptions.

Each tool retains its name, title, description and schemas. All four annotation hints come
from `Operation::annotations()`; administrative effects travel on the tool's `_meta` under
`io.github.thirtytwobits.clapham/effect`. rmcp represents output schemas as objects, so a
boolean schema is mapped to its exact object equivalent: `true` becomes `{}`, and `false`
becomes `{"not":{}}`.

`tools/call` returns the outcome's result as `structuredContent`, including an explicit JSON
null, and retains ordered content and `isError`. Refusals return `isError: true` with the
unchanged `Refusal` as `structuredContent` and its message as text. Protocol faults use
JSON-RPC errors. Set request `_meta["io.github.thirtytwobits.clapham/preview"]` to `true`
for a dry run; a non-boolean value is a protocol fault. Streaming records are drained into
a discard sink; only the final result is returned. An indefinite stream ends on cancellation.
MCP cancellation forwards to the core call token and rmcp suppresses the cancelled reply.
An ID named in a cancellation notification is retired for the connection. Reusing it closes
the transport, preventing a late completion from removing a newer call's accounting entry.
Retired IDs use fixed-size digests, bounded by the notification lifetime limit.

Image/audio data URIs with matching MIME type and base64 encoding become inline MCP blocks.
Other media URLs remain resource links without fetching them. Embedded resources retain
text/base64 contents and MIME types.

A query with a resource is listed as a resource template. Constant URIs are also listed by
`resources/list`. Templates support simple string variables such as `lists://{id}` and
`lists://{list_id}/items/{id}`. Variables name string properties in the input schema;
percent-encoded UTF-8 is decoded once. Operators, adjacent variables, ambiguous literal
separators and repeated variables refuse as `mcp_configuration`. Templates with identical
literal parts also refuse configuration, even when variable names differ. Other overlaps,
including a bare `{id}`, are detected when reading. An exact constant URI takes priority
over variable templates; multiple variable matches are a protocol fault.
`resources/read` calls the query through the same runner and returns JSON text with
`application/json`. A query refusal is returned as JSON resource contents with its code
unchanged. Plain successful results retain their JSON shape. Outcomes with content, an error
flag or no structured result are returned as the complete core `Outcome`, preserving those
semantics. Unknown or malformed URIs are protocol faults.

`tools/list`, resource lists and discovery are private and immediately stale (`ttlMs: 0`).
Lists paginate under `Bounds::message_bytes`, `catalogue_bytes`, `schema_bytes`, `tools` and
`pages`; cursors are bound to the admitted catalogue and list kind. The bounded transport
counts the full encoded frame, including lifecycle metadata, request ID and newline.
Oversized input lines are discarded without buffering their tail. An oversized successful
tool reply or resource outcome becomes `result_too_large` with uncertain evidence. Resource
reads reserve envelope space before encoding their contents; stdio checks the final frame.
Oversized tool error replies (including tool-reported error outcomes), existing refusals and
response envelopes close the connection rather than changing refusal codes or error shapes.
Replacement tool results retain the original response metadata. See the
[call contract](../../docs/calls.md#transport-generated-refusals) for transport refusal semantics.
Incoming request/notification totals and outstanding requests obey
`notifications` and `in_flight + waiting`; calls acquire an execution slot before dispatch.
Exhausting transport limits closes the connection.

The `stdio` feature provides `serve_stdio` and `serve_io` (the same transport over supplied
async streams). Consumers using another transport can use the `rmcp::ServerHandler`
implementation and must enforce their transport bounds. The `http` feature provides `HttpServer`, a Tower service using rmcp's stateless Streamable
HTTP server transport. It accepts POST requests without an initialise handshake, supplies
JSON replies, and never creates a session. The application owns the listener, TLS and shutdown.

```rust
# #[cfg(feature = "http")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use clapham::{Bounds, Principal, PrincipalKind, compose};
use clapham_mcp_server::{Credential, HttpServer};

let set = compose([], Bounds::default())?;
let server = HttpServer::new(set, |credential: &Credential| {
    match credential.authorization() {
        Some("Bearer configured-token") => Some(Principal::new("operator", PrincipalKind::Human)),
        _ => None,
    }
})?;
// Mount `server` as a Tower service at the application's MCP endpoint.
# Ok(())
# }
# #[cfg(not(feature = "http"))]
# fn main() {}
```

The application supplies `Authenticator`, which receives the complete Authorization value
as an opaque `Credential`. A missing field is a default credential; only the authenticator
may explicitly map it to a principal. Credential debug output reports presence without printing
the value. Duplicate or malformed Authorization fields receive HTTP 400. An unmapped credential
receives HTTP 401 with a `not_authenticated` refusal carrying `not_dispatched` evidence,
before body consumption or any operation. No authentication scheme is assumed: the host
may set `with_authentication_challenge` with a validated HTTP header value, including a
Basic realm or Bearer `resource_metadata` parameter. Without it, no WWW-Authenticate field
is emitted. Authentication repeats on
every request, including when requests overlap or reuse the same JSON-RPC ID. MCP metadata
cannot override the authenticated principal.

`HttpServer::select` narrows the shared surface as for stdio. Hosts default to rmcp's loopback
allow-list; `with_allowed_hosts` replaces it (an empty list disables host filtering). Origin
validation rejects every present Origin by default. `with_allowed_origins` permits named
browser origins; non-browser clients without Origin remain permitted by this check.
Host and Origin checks run before credential parsing and authentication, using rmcp's own
authority/origin policy; rejected requests never invoke the authenticator.
The transport requires MCP 2026-07-28 metadata and its corresponding `MCP-Protocol-Version`,
`Mcp-Method` and, for named requests, `Mcp-Name` headers. rmcp checks routing headers against
the body, including input properties annotated with `x-mcp-header`. GET and DELETE return 405.

HTTP requests share `Bounds::in_flight` execution slots and `in_flight + waiting` outstanding
slots across clones. Unlike stdio's operation slots, an HTTP execution slot covers the whole
request: body reading, discovery, dispatch and reply buffering. Discovery can therefore queue
behind long-running calls, and a slow authenticated upload occupies an execution slot.
A full queue receives HTTP 503 with `server_busy` and not-dispatched
evidence. Dropping a pending HTTP response future cancels the core call and releases its
slots. rmcp enforces request body bytes while reading, without relying on Content-Length;
oversized input receives HTTP 413. Catalogue, schema and reply budgets also apply. Oversized
successful tool replies and resource outcomes return `result_too_large`. An existing refusal
or tool error outcome that cannot fit produces a bounded HTTP 500 infrastructure error,
without substituting another operation refusal. Effects are uncertain when a reply is lost.

The conformance crate keeps `records/mcp-protocol.json`: the listed tools with schemas,
annotations and metadata, resource templates, protocol versions and the server-published
`REQUEST_META_KEYS` contract. Its comparison checks canonical pretty-printed bytes, including
formatting and key order. `cargo xtask check` clears `CLAPHAM_WRITE_RECORDS` in every child
Cargo command, so check mode never silently regenerates the record.
`cargo test -p clapham-conformance --test mcp_protocol` refuses a changed wire form.
After reviewing an intentional change, regenerate it with:

```text
CLAPHAM_WRITE_RECORDS=1 cargo test -p clapham-conformance --test mcp_protocol
```

The boundary gate permits rmcp server features only in this crate, checking defaults, direct
features and feature forwarding on normal, development, build and target-specific dependencies.

See the [MCP mapping and authentication guide](https://github.com/thirtytwobits/clapham/blob/v0.1.1/docs/mcp.md)
and [0.1.0 guarantees](https://github.com/thirtytwobits/clapham/blob/v0.1.1/docs/contract.md)
for the application-facing contract and corresponding tests.
