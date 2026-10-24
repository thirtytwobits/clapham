# Using a release

Clapham releases are Git tags on `main`. Choose an existing tag from the repository's
[releases](https://github.com/thirtytwobits/clapham/releases); examples use `v0.1.0`, the first
stable release. Choose a published tag for a consumer dependency. See the
[release procedure](../RELEASING.md) for tagging and publication.

## Depend on a crate by tag

```toml
[dependencies]
clapham = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }
clapham-cli = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }
clapham-mcp-client = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0", default-features = false, features = ["stdio"] }
```

Use the same repository URL and tag for every Clapham crate. Commit the consumer's
`Cargo.lock` to retain Cargo's resolved commit. The core and CLI require Rust 1.85; the MCP
crates and conformance library require Rust 1.88. MCP transport features are `stdio` and
`http`; both are enabled by default. Choose the features the application needs.

## Run conformance checks in another repository

```toml
[dev-dependencies]
clapham-conformance = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }
tokio = { version = "1", features = ["macros", "rt"] }
serde_json = "1"
```

Keep its tag identical to the production Clapham dependencies. Define a fresh contribution
factory, finite cases and a scripted gate, then call `clapham_conformance::run` from a Cargo
integration test. The [harness guide](../crates/clapham-conformance/README.md) documents the
process protocol and custom drivers; the [lists executable](../examples/lists/README.md)
demonstrates all six front ends. Application policy stays in the caller's contribution.

## Patch to a local checkout

Put the patch in the consumer's workspace root. Patch every Clapham crate in its graph,
including the conformance dev-dependency, to avoid mixing source identities:

```toml
[patch."https://github.com/thirtytwobits/clapham"]
clapham = { path = "../clapham/crates/clapham" }
clapham-cli = { path = "../clapham/crates/clapham-cli" }
clapham-mcp-server = { path = "../clapham/crates/clapham-mcp-server" }
clapham-mcp-client = { path = "../clapham/crates/clapham-mcp-client" }
clapham-conformance = { path = "../clapham/crates/clapham-conformance" }
```

The checkout's versions must satisfy the consumer's requirements, including any prerelease
suffix. Run `cargo update` and inspect the lockfile's local sources before testing. Remove
the patch and update the lockfile when returning to the released tag. A checkout supplies
its own Cargo workspace; no other build system or product repository is required.

See [release metadata](../release.json), the [changelog](../CHANGELOG.md),
[release procedure](../RELEASING.md) and [security policy](../SECURITY.md).
