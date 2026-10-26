# Conformance

Supply `Fixture::new` a fresh contribution factory, or `Fixture::configured` a fresh
`Configuration` factory, with finite invocation cases, bounds and scripted OAuth contexts.
Contributions are composed directly, with every operation exposed. A configuration registers
its providers, resource policies and exposures and binds its OAuth resource. Cases must cover
every composed operation with schema-valid input. Use explicit holds and releases for
concurrent behaviour.

`run` compares each front end with the independent in-process baseline. It checks discovery,
results, visible-operation refusal codes/categories/retryability/evidence, CLI exit codes and
ordered records where carried. A resource policy's refusal is compared like any other; its
evidence is `not_dispatched`. Namespace collisions are checked before drivers start.

`ProcessFrontEnd` drives a reference executable through the CLI against its daemon over MCP
stdio/HTTP, raw MCP stdio/HTTP and client-bound stdio/HTTP. The fixture protocol supplies controlled claim snapshots
through the host environment and scripted HTTP credentials. Production hosts verify OAuth
credentials. An operation hidden before CLI parsing refuses as `usage`; one hidden before
remote binding refuses as `mcp_selection`. These native selection failures are checked explicitly.

A host executable composes the same fixture with `Fixture::compose`.

The [lists fixture](../crates/clapham-conformance/src/harness/lists.rs) loads
[JSON configuration](../crates/clapham-conformance/src/harness/lists.json), registers providers
and resource policies and extends the configuration in Rust. The JSON attaches `lists.private`
to `lists.get`; the `items.tick` descriptor names `items.private`. The
[lists executable](../examples/lists/src/main.rs) composes that fixture. Its
[matrix](../examples/lists/tests/conformance.rs) checks all seven front ends, including different
grants for the same subject and both policies' refusals. The
[OAuth integration test](../crates/clapham-conformance/tests/oauth_operations.rs) uses a real
introspection exchange and a deterministic verification clock.

See [the library](../crates/clapham-conformance/README.md) and [contract](contract.md).
