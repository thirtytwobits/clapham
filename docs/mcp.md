# MCP

Both adapters use rmcp 3.6.0 and MCP 2026-07-28, with Rust 1.88. The `stdio` and `http`
features select transports; both are enabled by default. Core and CLI keep Rust 1.85.

## Server

The application supplies a composed set, selection, bounds and authenticated principal.
`Server` exposes stdio or supplied async streams; `HttpServer` is a service mounted by the
application, which owns its listener, TLS and shutdown. See [composing](composing.md) for
the worked application and [the release contract](contract.md#c10-front-ends-preserve-the-call-boundary)
for test evidence. The [server API guide](../crates/clapham-mcp-server/README.md) specifies
resource-template restrictions, encoded budgets and transport failure details.

### Descriptor and outcome mapping

| Clapham value | MCP representation |
|---|---|
| Complete operation name, title, description | Tool name, title, description; names are not rewritten |
| Input / optional output schema | `inputSchema` / `outputSchema`; boolean output schemas become equivalent objects |
| Effect, idempotence, open-world hint | Tool annotations; administrative class also appears on tool `_meta` |
| Structured result, including JSON null | `structuredContent`; absence remains absence |
| Ordered content and tool-error flag | MCP `content` blocks and `isError` |
| Typed refusal | `isError: true`, unchanged refusal as structured content, message as text |
| Query resource template | Resource discovery and `resources/read` through the ordinary runner |
| Streaming records | Discarded in 0.1.0; only the terminal result is returned |

Tool calls use `ComposedSet::call`, including selection, input, gate, preview and locality
checks. Protocol faults are JSON-RPC errors, distinct from domain refusals. A resource read
returns JSON text (`application/json`): ordinary results keep their shape; content/error/
absent-result outcomes use the complete `Outcome`. Query refusals retain their code in the
JSON resource contents. Indefinite streams require cancellation; MCP does not relay records.

### Metadata and discovery

| Key | Location and meaning |
|---|---|
| `io.github.thirtytwobits.clapham/effect` | Tool `_meta`; `administrative` preserves the effect class |
| `io.github.thirtytwobits.clapham/preview` | Request `_meta`; boolean `true` requests preview |
| `io.modelcontextprotocol/protocolVersion` | Request `_meta`; `2026-07-28` |
| `io.modelcontextprotocol/clientCapabilities` | Request `_meta`; client capability data |
| `io.modelcontextprotocol/clientInfo` | Request `_meta`; client identity data, not authentication |

The server publishes `PREVIEW_META_KEY` and `REQUEST_META_KEYS`. Non-boolean preview metadata
is a protocol error. Discovery lists only selected, gate-allowed operations for the caller;
denied and pending names disappear. Its internal named describe calls still require gate
admission for `clapham.describe`, even when the describe tool itself is not exposed.
Lists are private, immediately stale (`ttlMs: 0`) and paginated under shared bounds.
The gate must be safe to query repeatedly without beginning an approval workflow.

### Authentication and transport ownership

A stdio connection keeps its launcher-supplied principal. HTTP passes each request's opaque
Authorization credential to the application's `Authenticator`. Only an explicit mapping
admits a principal, including a missing credential if the host deliberately maps it.
An unmapped credential receives HTTP 401 before body consumption, gate or handler.
Authorization values are excluded from credential debug output. No scheme is assumed;
the host supplies any authentication challenge. Request arguments and MCP metadata cannot
replace the authenticated principal. Concurrent requests retain their separate identities.

HTTP is stateless: there is no session ID or initialise handshake. `MCP-Protocol-Version`,
`Mcp-Method` and, for named requests, `Mcp-Name` must match the body. Host and Origin policy
runs before authentication. Defaults permit loopback hosts, reject present browser Origins and permit
non-browser requests without Origin; the host can configure both allow-lists. HTTP GET and
DELETE receive 405. The service accounts for full request/reply bodies and shared active/
waiting slots; disconnection cancels admitted work with the core's evidence rules.
Core defaults still refuse daemon-local operations until the host composes in daemon mode.

### Published wire record

[`mcp-protocol.json`](../crates/clapham-conformance/records/mcp-protocol.json) records the
neutral lists contribution's protocol versions, metadata keys, tools and resource templates.
`cargo test -p clapham-conformance --test mcp_protocol` compares canonical bytes and refuses
stale output. An intended update is generated with `CLAPHAM_WRITE_RECORDS=1`; the normal
repository gate removes that variable and cannot silently rewrite the record. This is a
checked fixture surface, not a promise that every application exposes the lists operations.

## Client

The client was extracted from Battersea v0.17.0. [Provenance](../provenance.json) records the
source commit and paths; [NOTICE](../NOTICE) retains the source attribution. This section
carries forward the released MCP adapter contract with Clapham's contribution and call types.

### Ownership and discovery

The host supplies a stable server identity, a non-secret credential-scope revision, finite
`clapham::Bounds` and an operation timeout. The server identity is also the contribution's
namespace. Stdio accepts host-created streams and a cancellation token that signals the host
to reap its process. HTTP accepts a host-approved endpoint and bearer credential. Closing or
dropping the connection stops transport work. Contributions retain their connection until
released or explicitly closed.

Connections use `server/discover` and self-contained request metadata. Discovery reads fresh
pages and bounds page count, tool count, schema size and total catalogue bytes. Repeated
cursors are refused. Dropping an active discovery operation closes its connection. Invalid
records receive individual diagnostics. Duplicate remote names are invalid, including every
occurrence of the duplicate. Schemas are compiled offline without remote reference fetching.

Catalogue provenance retains the server's original record. Remote metadata cannot grant
principal access. The host selects tools and optionally assigns their permitted principal
kinds; omission uses the mapped effect's defaults. The application's gate remains authoritative.

### Bindings as contributions

`McpCatalogue::bind` pins selected remote names. Each `McpSelection` can supply a local alias;
the complete operation name joins the connection's namespace and that local name with a dot.
Names keep their spelling: dots and underscores remain distinct. Repeated selected names,
repeated aliases and invalid complete names are refused. Explicit aliases can shorten names
that would exceed the complete-name limit. Binding refuses a closed or failed connection.
`McpToolBinding::contribution` produces a
`Contribution`; composition checks namespace ownership and collisions atomically alongside
all local contributions.

A descriptor takes its title, description and schemas from the remote record. Records with
an invalid name or an absent or blank description are refused during discovery. A remote
administrative marker maps conservatively to `destructive`, never to `administrative`.
Otherwise the effect is `query` where `readOnlyHint` is true, then `destructive` where
`destructiveHint` is true, and `mutation` otherwise. The MCP default for an absent
`destructiveHint` is true, so unannotated tools default to `destructive` and exclude agents
unless the host explicitly authorises them. An explicit non-destructive hint permits
`mutation`. Idempotence follows the remote hint, with queries always projecting an idempotent
annotation. `open_world` follows `openWorldHint`, defaulting to true as MCP specifies.
Locality is `daemon`; compose with `ExecutionMode::Daemon` to dispatch. Remote contributions
expose no preview, record stream or resource template.

The binding revision hashes server identity, credential scope, selected original definitions,
aliases and exposed descriptors using canonical JSON. Composition includes that opaque revision
in selections that use the binding. A tool-list notification or a refresh that fails after it
starts makes existing bindings stale. A successful refresh permits an existing binding to
dispatch only while its selected definitions remain identical. Changing credentials requires
another connection and binding. Reconnecting and discovering never replay calls.

### Dispatch, refusals and results

The composed call runner validates arguments and admission before dispatch and validates
successful structured output before delivery. An operation deadline covers queueing and
invocation. Active and waiting calls are bounded. Queue refusals, invalid arguments and stale
bindings carry `not_dispatched` evidence. A completed response carries `completed` evidence,
including tool errors, JSON-RPC invocation errors and failed output validation. Lost replies,
disconnects and invocation deadlines carry `uncertain` evidence.

Each remote operation declares the same bounded adapter refusal set:

| Adapter error | Refusal code | Category |
|---|---|---|
| `Invocation(-32602)` | `mcp_invalid_input` | `invalid_input` |
| `Invocation(-32601)` | `mcp_not_found` | `not_found` |
| `Invocation(other)` | `mcp_invocation` | `internal` |
| `Timeout` | `mcp_timeout` | `interrupted`, retryable |
| `Transport` | `mcp_transport` | `unavailable` |
| `Busy` | `mcp_busy` | `unavailable` |
| `Limit` | `mcp_limit` | `unavailable` |
| `Stale` | `mcp_stale` | `conflict` |
| `Configuration` | `mcp_configuration` | `configuration` |
| `Selection` | `mcp_selection` | `usage` |
| `Protocol` | `mcp_protocol` | `internal` |
| `UnsupportedInteraction` | `mcp_unsupported_interaction` | `internal` |

`McpError::refusal` also maps connection and discovery errors for the host, using the evidence
it supplies. JSON-RPC diagnostics retain their numeric code in `details.json_rpc_code`, excluding
remote diagnostic text and data. The core's own validation and cancellation refusals retain
its [call contract](calls.md).

Calls use one `tools/call` round. Dropping the call future requests cancellation; cancellation
does not prove that remote effects stopped. HTTP redirects, POST retries, SSE reconnects and
session reinitialisation are disabled. The host reconciles uncertain effects before deciding
whether to issue another call. The issue #13 contract marks timeouts retryable even when
evidence is uncertain. This indicates a potentially temporary failure, not permission to
replay: callers must inspect evidence and reconcile effects before retrying an uncertain call.

Ordered text, image, audio, resource links and embedded resources map to `Outcome.content`.
Inline image and audio bytes become data URIs; resource references are retained without fetching.
The SDK normalises absent and empty remote content to `Outcome.content: Some([])`.
`structuredContent` maps to `Outcome.result`, keeping null distinct from an absent result.
`isError: true` sets `Outcome.is_error`, so its structured content does not undergo successful
output validation. A refusal inside that content keeps its original code; the client does not
lift it into a dispatch refusal. Remote result metadata has no field in `Outcome` and is omitted.

Sampling, roots, elicitation and tasks are unadvertised. Server requests for unsupported
interactions are rejected. `input_required` and task results stop execution with uncertainty;
they do not initiate follow-up calls. The host controls approval, durable effect intent and
reconciliation.

### Bounds

`Bounds` limits each stdio frame and the complete body of each HTTP response, including SSE.
The complete-response cap also applies to a persistent GET stream: events, keep-alive comments
and SSE framing share one `message_bytes` budget for that response. This can end the connection
before its separate notification-count or 64 MiB payload lifetime budgets are exhausted. The
host can reconnect and rediscover after a limit failure, without replaying uncertain calls.
The message limit ranges from 8 KiB to 8 MiB; 4 KiB is reserved for request metadata and framing.
Oversized frames fail the connection explicitly. Bounds also limit catalogue retention,
concurrent operations, waiting operations and incoming requests/notifications over the
connection's lifetime. Incoming request and notification payloads share a 64 MiB lifetime
budget, measured from received payload bytes including JSON whitespace. Queued and active
message capacity must fit within 64 MiB. A host can open a new connection after a lifetime
limit is reached. Credentials and response diagnostic data are excluded from adapter errors.

### Conformance

The moved client tests cover bounded discovery, duplicate names, aliases, binding revisions,
stale notifications and refreshes, unsupported interactions, cancellation, uncertain effects,
JSON/SSE responses and disabled replay. Fixtures use explicit acknowledgements and holds;
deadline fixtures use paused runtime time after a scripted request has entered. Once scripted
work is idle, the runtime advances to the production deadline without elapsed wall time.
The conformance suite binds this workspace's server over both
transports and calls `remote.lists.get` through
`ComposedSet::call`, checking the local result, a remote `list_not_found` tool error and
agent refusal for an administrative round trip. Response interpretation follows request IDs,
so tool-result extension fields cannot be mistaken for catalogue records.
