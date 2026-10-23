# clapham-mcp-server

`Server` exposes a composed set as MCP 2026-07-28 tools and resources, using rmcp 3.6.0.
The application supplies the principal when constructing the server; a stdio connection
keeps that identity for its lifetime. Request arguments and metadata cannot replace it.

```rust,no_run
use clapham::{ComposedSet, Principal};
use clapham_mcp_server::Server;

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
Gate admission is checked again at dispatch through `ComposedSet::call`.

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

Image/audio data URIs with matching MIME type and base64 encoding become inline MCP blocks.
Other media URLs remain resource links without fetching them. Embedded resources retain
text/base64 contents and MIME types.

A query with a resource is listed as a resource template. Constant URIs are also listed by
`resources/list`. Templates support simple string variables such as `lists://{id}` and
`lists://{list_id}/items/{id}`. Variables name string properties in the input schema;
percent-encoded UTF-8 is decoded once. Operators, adjacent variables, ambiguous literal
separators and repeated variables refuse as `mcp_configuration`. An exact constant URI
takes priority over variable templates; multiple variable matches are a protocol fault.
`resources/read` calls the query through the same runner and returns JSON text with
`application/json`. A query refusal is returned as JSON resource contents with its code
unchanged. Unknown or malformed URIs are protocol faults.

`tools/list`, resource lists and discovery are private and immediately stale (`ttlMs: 0`).
Lists paginate under `Bounds::message_bytes`, `catalogue_bytes`, `schema_bytes`, `tools` and
`pages`; cursors are bound to the admitted catalogue and list kind. The bounded transport
counts the full encoded frame, including lifecycle metadata, request ID and newline.
Oversized input lines are discarded without buffering their tail. An oversized successful tool reply
becomes `result_too_large` with uncertain evidence. Oversized error replies and response
envelopes close the connection rather than changing a refusal code. Incoming request/notification totals and outstanding requests obey
`notifications` and `in_flight + waiting`; calls acquire an execution slot before dispatch.
Exhausting transport limits closes the connection.

The `stdio` feature provides `serve_stdio` and `serve_io` (the same transport over supplied
async streams). Consumers using another transport can use the `rmcp::ServerHandler`
implementation and must enforce their transport bounds. The `http` feature is reserved;
Streamable HTTP, authentication and the generated wire record follow separately.
