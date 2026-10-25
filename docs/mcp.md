# MCP

Clapham's MCP adapters use rmcp 3.6.0 and MCP 2026-07-28. All library crates require Rust 1.88.
The core operation model has no rmcp dependency.

## Server

`Server` binds host-verified OAuth authority to a private stdio connection. `HttpServer` requires a composed OAuth resource binding and
authenticates every request using asynchronous middleware. `OAuthIntrospector` delegates
RFC 7662 requests to the OAuth library and checks active status, issuer, audience, subject,
expiry and not-before. Introspection extension claims remain available to resource policy.

`into_router` mounts MCP at `/mcp` and the protected-resource metadata. The application
supplies its listener, TLS and shutdown. Host/Origin validation and MCP protocol
handling use rmcp. Bounds limit input/output, catalogue sizes, active requests and waiting work.

Tool definitions and annotations derive from operations. Scope requirements and host policy
control visibility. Calls additionally validate typed input and run resource policies. Domain
refusals retain their codes in structured tool errors. Native rmcp content blocks stay inside
the adapters; the core carries opaque content. Result metadata carries explicit effect evidence
under `io.github.thirtytwobits.clapham/evidence`, and under `io.github.thirtytwobits.clapham/absent`
the outcome fields that are absent: `content`, which MCP's required content list cannot omit, and
`result`, whose absence differs from a JSON null.

`HttpServer::handle` bounds a JSON reply as a whole. It streams an SSE reply as rmcp writes it,
each event bounded, and holds the request's queue and execution permits until the stream ends.

## Records extension

The records extension, `io.github.thirtytwobits.clapham/records`, carries a streaming
operation's records over MCP. The server declares it in its `server/discover` capabilities and
the client in each request's client capabilities. A tool whose operation streams carries `true`
under the extension identifier in its `_meta`.

- A client that declares the extension sends a records token in the `tools/call` request's
  `_meta` under the extension identifier. A token in use by an in-flight call refuses the call
  with `-32602`.
- The server sends each record as an `io.github.thirtytwobits.clapham/record` notification on
  the call's response stream, with params `{token, sequence, record}` and `sequence` from 1,
  then the final result.
- The client acknowledges each record with an `io.github.thirtytwobits.clapham/acknowledge`
  request, with params `{token, sequence}`, after its sink accepts the record. The server sends
  the next record only after the acknowledgement. Only the grant that began the call may
  acknowledge its records. Repeating the acknowledgement of the record last accepted succeeds
  while the call is in flight.
- Acknowledgements are admitted outside the request queue and execution permits, and over stdio
  outside the active-request and lifetime counts. Over Streamable HTTP, a request whose
  `Mcp-Method` names the acknowledgement answers nothing else.
- The client repeats an acknowledgement once after a transport failure. An acknowledgement it
  cannot confirm, a sink refusal or a record out of sequence cancels the call with uncertain
  evidence.
- The record sink refuses a notification over `bounds.message_bytes` with `result_too_large`
  and an unacknowledged record with `record_delivery_failed`, both with uncertain evidence. A
  handler that returns the refusal carries it to the caller unchanged, without declaring it.
  Closing the response stream, or `notifications/cancelled` over stdio, cancels the call.
- A client that does not declare the extension, or sends no token, receives the final result
  alone.

The client bounds each SSE event to `bounds.message_bytes` with its framing, and an SSE reply
to one event beyond the notifications it has admitted. Acknowledgement paces a call's records,
so the record a call awaits is outside the connection's lifetime notification and byte budgets;
a record that arrives while the call holds another is counted.

## CLI daemon mode

The CLI's daemon mode is Clapham's MCP client. `McpBackend` reads the catalogue from
`clapham.describe` and calls each operation as `tools/call`, with its input as arguments and
`--dry-run` as `io.github.thirtytwobits.clapham/preview` in request `_meta`. Records arrive under
the records extension and print as NDJSON, each acknowledged after the output sink accepts it.
A tool result that carries a structured refusal is that refusal. Cancelling closes the call's
response stream, or sends `notifications/cancelled` over stdio.

`McpConfig::operator` sizes the connection for an operator: the largest message `Bounds`
allows and one call at a time. `McpConnection::call_tool` has no deadline: the CLI's
`--timeout` and cancellation bound each call, and a `tools/call` request over Streamable HTTP
has no request deadline of its own. The CLI obtains tokens through the application's OAuth
client or credential store and cannot choose identity in arguments. A credential the server
does not accept refuses the connection with `mcp_unauthenticated`.

## Remote connections

The host supplies approved upstream endpoints or process streams, OAuth credentials scoped
to the upstream audience, finite bounds and explicit local permission requirements. Remote
metadata cannot grant authority. Configure required scopes on `McpSelection`; effect hints
remain behavioural information. Register the resulting contribution through the shared
configuration's connection registry or ordinary trusted composition.

Bindings pin definitions, aliases and credential-scope revisions. Tool-list notifications and
failed refreshes invalidate stale bindings. Call deadlines, lost replies and disconnects
preserve uncertainty. Redirects, automatic replay and session reinitialisation remain disabled
by the bounded client. Scope changes require a new authenticated connection/binding.

See [the client library](../crates/clapham-mcp-client/README.md),
[application guide](composing.md) and [contract](contract.md).

Register resource URI templates on `Server::resources` or `HttpServer::resources` against
query operations. The adapter validates templates and derives native rmcp annotations from
operation behaviour. Resource registration belongs to the MCP adapter.

## Protocol facts

Clapham's MCP design relies on these facts. The
[server fact tests](../crates/clapham-mcp-server/src/rmcp_facts.rs) and
[client fact tests](../crates/clapham-mcp-client/tests/rmcp_facts.rs) pin each rmcp row.
rmcp source paths are relative to the rmcp 3.6.0 crate root.

### MCP 2026-07-28

| Fact | Source |
|---|---|
| A server may answer any request with an SSE stream scoped to that request, carrying notifications before the final response. The notifications must relate to the originating request; `notifications/progress` and `notifications/message` are examples, not a closed set. | [Streamable HTTP: Receiving Messages](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http#receiving-messages) |
| Every transport carries the same patterns: a request is answered by a response, optionally preceded by notifications scoped to it. stdio carries them as newline-delimited JSON-RPC on one shared channel. | [Message patterns](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns), [stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio) |
| Messages on one request's stream arrive in the order sent, because the stream is a single SSE response or a single stdio pipe. | follows from the two rows above |
| Closing a request's SSE stream cancels that request; on stdio the client sends `notifications/cancelled`. | [Streamable HTTP: Cancellation](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http#cancellation), [stdio: Cancellation](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio#cancellation) |
| Servers do not initiate requests. Server-to-client interactions are input requests inside an `InputRequiredResult` (MRTR). | [Message patterns](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns), [SEP-2322](https://modelcontextprotocol.io/seps/2322-MRTR.md) |
| The core protocol defines no client-to-server notifications over Streamable HTTP. A server that accepts a notification POST answers `202 Accepted` with no body. | [Streamable HTTP: Sending Messages](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http#sending-messages) |
| Extensions are identified as `{vendor-prefix}/{extension-name}` and declared under an `extensions` map in client and server capabilities. A party whose peer lacks an extension reverts to core behaviour or rejects the request. SEP-2133 is Final. | [SEP-2133](https://modelcontextprotocol.io/seps/2133-extensions.md), [`ClientCapabilities.extensions`](https://modelcontextprotocol.io/specification/2026-07-28/schema#clientcapabilities-extensions), [`ServerCapabilities.extensions`](https://modelcontextprotocol.io/specification/2026-07-28/schema#servercapabilities-extensions) |
| A client declares its capabilities in each request's `_meta` under `io.modelcontextprotocol/clientCapabilities`; servers must not infer them from earlier requests. A server declares its capabilities in `DiscoverResult`. | [`RequestMetaObject`](https://modelcontextprotocol.io/specification/2026-07-28/schema#requestmetaobject-iomodelcontextprotocolclientcapabilities), [`DiscoverResult`](https://modelcontextprotocol.io/specification/2026-07-28/schema#discoverresult-capabilities) |
| A progress value must increase with each notification; progress carries a number, an optional total and an optional human-readable message, and both sides should rate-limit it. Progress notifications are not a record channel. | [Progress](https://modelcontextprotocol.io/specification/2026-07-28/basic/patterns/progress.md) |
| SEP-2998, partial tool results, was closed on 22 September 2026 without acceptance; new SEPs require a Working Group. | [modelcontextprotocol#2998](https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2998) |

### rmcp 3.6.0

| Fact | Source |
|---|---|
| A server handler sends a request-scoped notification, including a `CustomNotification { method, params }`, through `RequestContext::peer`. `Peer::send_notification` returns once the transport has written the message, so a handler that awaits each send writes its notifications in order before its response. | `src/service.rs` (`RequestContext`, `Peer::send_notification`, the serve loop's `PeerSinkMessage::Notification` arm), `src/model.rs` (`CustomNotification`) |
| The stateless Streamable HTTP server prefers a JSON reply, and falls back to SSE when the handler emits a notification before its response. | `src/transport/streamable_http_server/tower.rs` (`StreamableHttpServerConfig::json_response`, `serve_negotiated_request_directly`) |
| A client receives custom notifications through `ClientHandler::on_custom_notification`. `NotificationContext` carries no originating request id, so a request-scoped notification must carry its own correlation key. | `src/handler/client.rs`, `src/service.rs` (`NotificationContext`) |
| The serve loop runs each incoming notification's handler in its own task, so handlers are not ordered with one another or with the response. | `src/service.rs` (the serve loop's `JsonRpcMessage::Notification` arm, `spawn_service_task`) |
| The stateless server answers a client notification `202 Accepted` and drops it without calling the handler. | `src/transport/streamable_http_server/tower.rs` (`ClientJsonRpcMessage::Notification` in the stateless path of `handle_post`) |
| Custom requests reach `ServerHandler::on_custom_request`. The serve loop runs each request's handler in its own task, so a custom request is answered while a `tools/call` is held, including on the same stdio connection. | `src/handler/server.rs`, `src/service.rs` (the serve loop's `JsonRpcMessage::Request` arm) |
