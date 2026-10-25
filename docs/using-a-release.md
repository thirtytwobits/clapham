# Depending on Clapham

Clapham is under development. Use a shared checkout or pin the same repository revision for
all required Clapham crates, and commit the consumer's Cargo.lock. All libraries require
Rust 1.88; repository gates use Rust 1.99.0.

The core operation model has no MCP dependency. Add `clapham-cli` for generated command lines,
`clapham-mcp-server` for MCP/operation HTTP and `clapham-mcp-client` for remote contributions.
MCP transport features are `stdio` and `http`; both are enabled by default.

Use `clapham-conformance` as a development dependency to compare fresh application fixtures
through selected adapters. See [conformance](conformance.md), [composition](composing.md) and
[contributing](../CONTRIBUTING.md).
