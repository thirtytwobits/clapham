# Depending on Clapham

Clapham is under development. Use a shared checkout or pin the same repository revision for
all required Clapham crates, and commit the consumer's Cargo.lock. All libraries require
Rust 1.88; repository gates use Rust 1.99.0.

The core operation model has no MCP dependency. Add `clapham-cli` for generated command lines,
`clapham-mcp-server` for MCP and `clapham-mcp-client` for remote contributions.
MCP transport features are `stdio` and `http`; both are enabled by default.

Use `clapham-conformance` as a development dependency to compare fresh application fixtures
through selected adapters. See [conformance](conformance.md), [composition](composing.md) and
[contributing](../CONTRIBUTING.md).

<!-- newsletter:begin consuming · maintained in thirtytwobits/london -->

## Taking a release

Clapham publishes nothing to crates.io. Take its crates from a release tag:

```toml
[dependencies]
CRATE = { git = "https://github.com/thirtytwobits/clapham", tag = "vX.Y.Z" }
```

Cargo treats each tag as a separate source, so take every Clapham crate from the
same tag, and commit `Cargo.lock`. The libraries need Rust 1.88 or later.

`SHA256SUMS` lists the files attached to a release. Check a download with
`sha256sum --check --ignore-missing SHA256SUMS`.

[`london.toml`](https://github.com/thirtytwobits/london/blob/main/london.toml) names the release
of each piece that the London Project's examples build against.

<!-- newsletter:end consuming -->
