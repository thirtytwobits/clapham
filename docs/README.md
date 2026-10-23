# Workspace guide

| Member | Responsibility | Permitted workspace dependencies |
|---|---|---|
| `clapham` | Descriptors, refusals, principals, composition, selection, gates and calls | None |
| `clapham-cli` | Command line | `clapham` |
| `clapham-mcp-server` | MCP server | `clapham` |
| `clapham-mcp-client` | MCP client | `clapham` |
| `clapham-conformance` | Lists-and-items contribution and conformance harness | The four libraries above |
| `clapham-lists` | Example binary assembling the front ends over the contribution | Command line, MCP server and conformance library |
| `xtask` | Repository gates and schema generation | `clapham` |

The core exports the contract, contribution, composition, selection, gate and call types,
with compiled schema validation and deterministic revisions. See [composition](composition.md)
for the boundaries and [calls](calls.md) for admission, dispatch and refusal handling.
Generated schemas are in `crates/clapham/schemas/`.
The other four libraries and the example remain stubs. Each library has a generated
public API snapshot in `api/`.

`cargo xtask schemas` generates the operation and refusal schemas with schemars.
`cargo xtask schemas --check` refuses missing or stale files without rewriting them;
it runs within `check` on both CI runners.

`tools/xtask` owns the boundary policy. A new workspace member requires an explicit policy.
The gate reads Cargo metadata, so a dependency alias or target condition cannot conceal a
forbidden edge. Dependencies named `battersea-*`, `clerkenwell-*` or `walthamstow-*` are
refused in every workspace member.

CI runs `check` and `boundaries` on Ubuntu 26.04 and Windows 2025 with Rust 1.99.0. Separate
lanes check and test the core on Rust 1.85.0, the MCP tests and transport feature combinations
on Rust 1.88.0, and API snapshots on both runners with nightly-2025-08-02 and cargo-public-api
0.50.2. The only workflow action is `actions/checkout`, pinned to the v7.0.1 commit; its
release's `action.yml` declares the Node 24 runtime (verified when adding the workflow).
Rust commands themselves use Cargo and rustup.
