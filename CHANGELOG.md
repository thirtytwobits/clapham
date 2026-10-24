# Changelog

One section per release. Contract changes belong under `### Breaking`.

## [0.1.0-rc.1]

### Breaking

Initial release candidate; there is no earlier Clapham release contract. The MCP client
extraction replaces its source library's model and tool-registry integration with Clapham
contributions, outcomes, refusals and shared bounds. See `provenance.json` for the source
commit, paths and adaptations.

### Added

- Operation descriptors, principals, composition, selection, admission gates and calls, with
  bounded validation, revisions, preview and explicit dispatch evidence.
- Descriptor-derived command line with input binding, formats, confirmations and exit codes.
- MCP server over stdio and stateless Streamable HTTP, with authenticated discovery and calls.
- MCP client binding remote tools as daemon contributions over either transport.
- Neutral lists-and-items contribution, reference executable and reusable conformance harness
  covering all six front ends, refusals, discovery, gate visibility and ordered records.
- Generated schemas, wire record and public API snapshots, with Cargo repository gates.
- The 0.1.0 contract with named test evidence, a compiling application walkthrough, and
  command-line, MCP and conformance usage guides.
- Release metadata and package inventories, private security reporting and a gated workflow
  that creates draft releases from tags on `main`.
