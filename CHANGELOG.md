# Changelog

One section per release. Contract changes belong under `### Breaking`.

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
