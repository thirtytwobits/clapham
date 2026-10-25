# Changelog

## [0.2.0]

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

## [0.1.1]

### Breaking

Documentation changes only.

### Documentation

[API reference for 0.1.1](https://thirtytwobits.github.io/clapham/v0.1.1/) covers all five library
crates with all features enabled.

## [0.1.0]

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
