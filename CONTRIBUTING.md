# Contributing

Read [AGENTS.md](AGENTS.md) before changing the repository. Cargo selects the gate
compiler from `rust-toolchain.toml`.

```console
cargo xtask check
cargo xtask boundaries
cargo xtask api
cargo xtask schemas --check
```

`check` runs dependency, crate-file and generated-schema checks, formatting, Clippy with
warnings denied, tests and documentation with warnings denied, for default features and
all features.
`boundaries` checks each declared dependency, including aliases, development and build
dependencies, optional dependencies and dependencies for other targets. No command needs
Node or another build system.

Install the API tools once:

```console
rustup toolchain install nightly-2025-08-02 --profile minimal
cargo +1.99.0 install cargo-public-api --version 0.50.2 --locked
```

`api/toolchain.json` records the API tool versions. `cargo xtask api` refuses missing or
stale snapshots and an unexpected cargo-public-api version. Accept an intentional API
change with `cargo xtask api --update`, review the resulting `api/*.txt` diff and commit it.
The command runs from any directory in the workspace and normalises line endings.

Check the minimum supported Rust versions with the committed lockfile:

```console
cargo +1.85.0 check --locked -p clapham -p clapham-cli
cargo +1.85.0 test --locked -p clapham -p clapham-cli
cargo +1.85.0 test --locked -p clapham --features serde_json/preserve_order revision_
cargo +1.88.0 test --locked -p clapham-mcp-server -p clapham-mcp-client -p clapham-conformance --all-features
cargo +1.88.0 check --locked -p clapham-mcp-server -p clapham-mcp-client --no-default-features
cargo +1.88.0 check --locked -p clapham-mcp-server -p clapham-mcp-client --no-default-features --features stdio
cargo +1.88.0 check --locked -p clapham-mcp-server -p clapham-mcp-client --no-default-features --features http
```

Generate the public operation, refusal and catalogue schemas and the CLI output envelope
with `cargo xtask schemas`. Review and commit changes to `crates/*/schemas/*.json`;
`cargo xtask schemas --check` checks them without writing. The schemas use JSON Schema
2020-12. Tests validate descriptors and refusals against the checked-in files.

The MCP crates reserve `stdio` and `http` features for their transports. The neutral
conformance contribution and CLI are implemented, including process output and safeguards.
The MCP front ends and the example follow in their own implementation issues. The MCP
wire-form generator follows with the server implementation.

See [the workspace guide](docs/README.md) for the crate boundaries and CI lanes.
