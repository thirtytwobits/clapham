# Repository constraints

Every section is a hard constraint.

## Independence and vocabulary

Clapham depends on no other London Project piece. Nothing under `crates/` names an
application, a product or another piece's internals. Fixtures use lists and items.

## Build and compatibility

Rust builds with Cargo alone. Do not introduce another build system or a Node dependency
into a Rust build. The gate toolchain is Rust 1.99.0.

`clapham` keeps Rust 1.85. `clapham-mcp-server` and `clapham-mcp-client` keep Rust 1.88,
which rmcp 3.6.0 requires. Keep the dependency boundary policy in `tools/xtask` explicit;
normal, development, build, optional and target-specific dependencies obey it alike.

## Public contracts

A crate exports exactly what its snapshot under `api/` records. Generate snapshots with
`cargo xtask api --update` using the recorded toolchain; review every change. Never edit
a generated snapshot by hand. Published schemas and the wire-form record must be written
by a gate, and that gate must refuse stale output when those artefacts are introduced.

A refusal code is carried unchanged. A contribution's declared refusal set is a contract.
`docs/contract.md` records release guarantees and their test evidence. A change to a guarantee
updates that document and its corresponding test in the same pull request.

## Deterministic tests

Tests never sleep, set a timer, read the wall clock or retry. A handler that must be caught
in flight is scripted: the test holds it and releases it. Use explicit synchronisation,
not elapsed time, to prove concurrent behaviour.

## Documentation and removal

Documentation is en-GB, precise and concise. A removal is clean: remove obsolete code,
tests, documentation and configuration together; do not leave compatibility debris.

## Validation

Run `cargo xtask check`, `cargo xtask boundaries` and `cargo xtask api` before handing off
a change. Keep the core and MCP minimum-version checks in CI passing. A changed public
API requires regenerated snapshots in the same change.
