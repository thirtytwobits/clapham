# Conformance

Supply a fresh contribution factory, finite invocation cases, bounds and scripted OAuth
contexts to `Fixture`. Cases must cover every contributed operation with schema-valid input.
Use explicit holds and releases for concurrent behaviour.

`run` compares each front end with the independent in-process baseline. It checks discovery,
results, visible-operation refusal codes/categories/retryability, CLI exit codes and ordered
records where carried. Namespace collisions are checked before drivers start.

`ProcessFrontEnd` drives a reference executable through the server-backed CLI, raw MCP
stdio/HTTP and client-bound stdio/HTTP. The fixture protocol supplies controlled claim snapshots
through the host environment and scripted HTTP credentials. Production hosts verify OAuth
credentials. An operation hidden before CLI parsing refuses as `usage`; one hidden before
remote binding refuses as `mcp_selection`. These native selection failures are checked explicitly.

The [lists executable](../examples/lists/src/main.rs) loads JSON configuration, registers
providers and extends the configuration in Rust. Its [matrix](../examples/lists/tests/conformance.rs)
checks all six front ends, including different grants for the same subject. The
[OAuth integration test](../crates/clapham-conformance/tests/oauth_operations.rs) uses a real
introspection exchange and a deterministic verification clock.

See [the library](../crates/clapham-conformance/README.md) and [contract](contract.md).
