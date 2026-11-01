# Changelog

## Unreleased

### Breaking

- The crates need Rust 1.95 or later.

## 0.3.3

### Added

- Each release attaches its API reference, `clapham-api-vVERSION.tar.gz`, and the `SHA256SUMS`
  of its files.

### Documentation

- `RELEASING.md` holds the release procedure the London Project shares among its pieces.

[API reference for 0.3.3](https://thirtytwobits.github.io/clapham/v0.3.3/) covers all five library
crates with all features enabled.

## 0.3.2

### Breaking

`docs/contract.md` revises C12.

- A `resources/read` for a URI that no registered template matches answers JSON-RPC `-32602`
  (Invalid Params) with the URI under `data.uri`, where it answered `-32002`. MCP 2026-07-28
  requires `-32602`.
- `Case` is no longer `UnwindSafe` or `RefUnwindSafe`.

### Added

- A resource template may end with one RFC 6570 form-style query expression. `{?a,b}` binds
  optional string properties and `{?name*}` binds every other query parameter into an optional
  object property whose values are strings. Registration refuses a query variable that names a
  required or non-string property or a name another expression binds, and a query template
  beside a template of its path alone. Path-only templates are unchanged.
- `Case::compare` sets a projection that the conformance harness applies to the baseline's
  successful result and to each front end's MCP result or CLI data before comparing them. A
  case without a projection compares its whole result.

### Documentation

`docs/mcp.md` records the accepted template forms and the MCP and rmcp facts they rely on.
`docs/conformance.md` states when a case projects its result.

[API reference for 0.3.2](https://thirtytwobits.github.io/clapham/v0.3.2/) covers all five library
crates with all features enabled.

## 0.3.1

### Breaking

The conformance harness compares resource-policy refusals across front ends. `docs/contract.md`
revises C12. The changes are confined to `clapham-conformance`.

- `Fixture::contributions` is replaced by `Fixture::compose`, which composes fresh state within
  the fixture's bounds under the caller's options. A host executable composes its fixture
  through it.
- `run` compares refusal evidence; a front end that changes a refusal's evidence fails.
- `lists_fixture` composes a configuration bound to the `https://identity.example.com` issuer
  and `https://lists.example.com` audience. It adds the list `private`, belonging to `owner`, and
  four cases. Its configuration attaches the `lists.private` resource policy to `lists.get`, and
  its `items.tick` descriptor names `items.private` and declares `list_access_denied`.
  `ListsHandle::contributions` is unchanged.

### Added

- `Fixture::configured` composes a `Configuration` factory with its providers, resource
  policies, exposures and OAuth resource binding. `Fixture::new` composes contributions directly.

### Documentation

The lists JSON configuration moves from `examples/lists` to `clapham-conformance`, where the
lists fixture and the reference executable share it.

[API reference for 0.3.1](https://thirtytwobits.github.io/clapham/v0.3.1/) covers all five library
crates with all features enabled.

## 0.3.0

### Breaking

The CLI calls its daemon over MCP, and the operation HTTP interface is removed. MCP carries
ordered records under the records extension. `docs/contract.md` revises C02, C05, C06, C08 and
C10 to C12.

- Operation HTTP is removed: `HttpServer::handle_operations` and the `/operations`, `/call` and
  `/ack/{ticket}` routes. `into_router` mounts `/mcp` and the protected-resource metadata.
- `clapham_cli::HttpBackend` is replaced by `McpBackend`, which connects over Streamable HTTP
  with `McpBackend::http` or over a launched daemon's standard streams with
  `McpBackend::stdio`. `clapham-cli` depends on `clapham-mcp-client`.
- `HttpServer::handle` returns a boxed body, and `HttpServer::Response` changes with it, so
  that an SSE reply streams as rmcp writes it.
- `McpError` gains `Unauthenticated` and `Forbidden`. A rejected credential refuses the
  connection as `mcp_unauthenticated`; a binding receives `mcp_forbidden` for a server's 403 and
  `mcp_busy` for its 503, with not-dispatched evidence, where these were `mcp_transport`.
- A record-sink refusal that a handler returns reaches the caller unchanged instead of becoming
  `undeclared_refusal`.
- `ProcessKind` gains `CommandLineStdio`; the conformance suite drives seven front ends.

### Added

- The records extension, `io.github.thirtytwobits.clapham/records`. The server sends each
  record as a notification on the call's response stream and the next only after the client
  acknowledges it; acknowledgements are idempotent and admitted outside the request limits.
  Bound remote tools pass records to the call's sink. A client without the extension receives
  the final result alone.
- The catalogue extension, `io.github.thirtytwobits.clapham/catalogue`, which pages the
  descriptors admitted to the caller whether or not the server's selection exposes
  `clapham.describe`.
- `RECORDS_EXTENSION`, `RECORD_NOTIFICATION`, `ACKNOWLEDGE_REQUEST` and `CATALOGUE_EXTENSION`
  in `clapham-mcp-server`, which answers both extensions' requests through
  `Server::on_custom_request`.
- `McpConnection::call_tool` and `McpConnection::operations` for calls and discovery outside a
  binding, and `McpConfig::operator`, which sizes a connection for an operator such as the CLI.
  A direct call has no deadline; its caller bounds it.
- Tool-result metadata names absent outcome fields under `io.github.thirtytwobits.clapham/absent`
  and marks refusals under `io.github.thirtytwobits.clapham/refusal`, so MCP preserves absent
  content, an absent result and an error outcome whose result has a refusal's shape.
- The reference executable's CLI calls its daemon over stdio with `CLAPHAM_TRANSPORT=stdio`.

### Documentation

`docs/mcp.md` records the MCP 2026-07-28 and rmcp 3.6.0 facts Clapham relies on, with tests
pinning each rmcp fact.

[API reference for 0.3.0](https://thirtytwobits.github.io/clapham/v0.3.0/) covers all five library
crates with all features enabled.

## 0.2.0

### Breaking

Operations are defined with typed Rust inputs, outputs and handlers, and authority comes from
verified OAuth grants. `docs/contract.md` records the revised guarantees C01 to C12.

- Principals are removed: `Principal`, `PrincipalKind`, effect-derived admission defaults,
  `clapham_cli::process_principal` and the CLI `--as` flag. `Call::new`, `Cli::new` and
  `Server::new` take an `OAuthContext`; `Invocation` no longer carries a principal.
- Admission enforces the OAuth resource binding, every required scope and application resource
  policies. Discovery checks operation-level authority; calls also check resource access.
- `Authenticator::authenticate` is asynchronous and returns an `OAuthContext`.
  `McpSelection::principals` is replaced by `McpSelection::scopes`.
- `clapham::ContentBlock` is removed. The operation core carries opaque content; the MCP
  adapters use rmcp's native content types.
- The CLI runs against the authenticated service through a `Backend` instead of in-process.
- `clapham` and `clapham-cli` require Rust 1.88.

### Added

- `TypedOperation` and `TypedOutcome`, which generate schemas and wire conversion from Serde and
  schemars types. Conversion failures are reported separately from application refusals.
- `Configuration`, one atomic composition path for declarative JSON, Rust builders and JSON
  extended programmatically, with a published configuration schema.
- `OAuthIntrospector` for bounded RFC 7662 token introspection, `ScopeGate` and
  `ResourcePolicy`. Token acquisition, refresh and credential storage remain host responsibilities.
- `HttpBackend` for the CLI, with bounded record streaming, one-use authenticated
  acknowledgements, cancellation and effect evidence.
- A typed-operation example and OAuth conformance coverage.

### Documentation

[API reference for 0.2.0](https://thirtytwobits.github.io/clapham/v0.2.0/) covers all five library
crates with all features enabled.

## 0.1.1

### Breaking

Documentation changes only.

### Documentation

[API reference for 0.1.1](https://thirtytwobits.github.io/clapham/v0.1.1/) covers all five library
crates with all features enabled.

## 0.1.0

### Breaking

Initial stable release; there is no earlier Clapham release contract. The MCP client
extraction replaces its source library's model and tool-registry integration with Clapham
contributions, outcomes, refusals and shared bounds. See `provenance.json` for the source
commit, paths and adaptations.

### Added

- Operation descriptors, principals, composition, selection, admission gates and calls, with
  bounded validation, revisions, preview and explicit dispatch evidence.
- Clapham-owned `Principal { id, kind }` and a default gate that admits descriptor principal
  kinds; the defaults exclude agents from administrative and destructive operations until
  the application admits them.
- Descriptor-derived command line with input binding, formats, confirmations and exit codes.
- MCP server over stdio and stateless Streamable HTTP, with authenticated discovery and calls.
- MCP client binding remote tools as daemon contributions over either transport.
- Neutral lists-and-items contribution, reference executable and reusable conformance harness
  covering all six front ends, refusals, discovery, gate visibility and ordered records.
- Generated schemas, wire record and public API snapshots, with Cargo repository gates.
- The 0.1.0 contract with named test evidence, a compiling application walkthrough, and
  command-line, MCP and conformance usage guides. Documentation gates check evidence
  references and keep the walkthrough's Rust example aligned with the core README doctest.
- Release metadata and package inventories, private security reporting and a gated workflow
  that creates draft releases from tags on `main`.

### Scope

The command line runs workspace operations in-process; daemon routing is deferred.
MCP returns final results and discards streaming records in 0.1.0. Applications own workspace
locks, persistence, transactions, authentication, approval coordination and shutdown.

### Documentation

[API reference for 0.1.0](https://thirtytwobits.github.io/clapham/v0.1.0/) covers all five library
crates with all features enabled.
