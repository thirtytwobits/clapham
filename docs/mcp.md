# MCP and operation HTTP

Clapham's MCP adapters use rmcp 3.6.0 and MCP 2026-07-28. All library crates require Rust 1.88.
The core operation model has no rmcp dependency.

## Server

`Server` binds host-verified OAuth authority to a private stdio connection. `HttpServer` requires a composed OAuth resource binding and
authenticates every request using asynchronous middleware. `OAuthIntrospector` delegates
RFC 7662 requests to the OAuth library and checks active status, issuer, audience, subject,
expiry and not-before. Introspection extension claims remain available to resource policy.

`into_router` mounts MCP, protected-resource metadata and the operation HTTP interface. The
application supplies its listener, TLS and shutdown. Host/Origin validation and MCP protocol
handling use rmcp. Bounds limit input/output, catalogue sizes, active requests and waiting work.

Tool definitions and annotations derive from operations. Scope requirements and host policy
control visibility. Calls additionally validate typed input and run resource policies. Domain
refusals retain their codes in structured tool errors. Native rmcp content blocks stay inside
the adapters; the core carries opaque content. Explicit effect evidence is carried in result
metadata under `io.github.thirtytwobits.clapham/evidence`.

## Operation HTTP and CLI

The non-MCP HTTP interface uses the same authenticator and runner:

- `GET /operations` returns the caller's admitted descriptors.
- `POST /call` accepts `{name,input,preview}` and streams bounded NDJSON
  `{kind:"record"|"outcome"|"refusal",data:...}` frames. Each record also carries a one-use
  `ack` ticket.
- `POST /ack/{ticket}` acknowledges sink acceptance, with an empty body and the same verified
  OAuth authority. ACK bodies must be complete and empty. Successful acknowledgement returns 204.

ACK tickets bind the complete verified grant, including scopes and resource restrictions.
An ACK must retain the authority recorded when the call began.

The CLI acknowledges each record after its sink accepts it. The producer waits for that
authenticated acknowledgement. Pending tickets and buffering are finite, record order is
preserved and disconnect cancels the call. This interface carries records to the generated
CLI; MCP progress notifications serve a different purpose. The CLI obtains tokens through
the application's OAuth client or credential store and cannot choose identity in arguments.

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
