# clapham-conformance

Deterministic lists/items fixtures and a reusable cross-adapter conformance harness.
Requires Rust 1.88.

`Fixture` supplies a fresh contribution or `Configuration` factory, finite `Case` values,
bounds and scripted OAuth contexts. `run` compares every selected front end with an independent
`InProcess` baseline. It checks results, domain refusals and their evidence, exit codes,
discovery and ordered records on record-bearing interfaces. Namespace collisions are checked
before drivers start. A host executable composes the fixture with `Fixture::compose`.

`ProcessFrontEnd` drives the executable, raw MCP stdio/HTTP and client-bound stdio/HTTP.
The executable CLI calls its authenticated operation service. An operation hidden during
CLI discovery fails as `usage`; a hidden remote binding fails as `mcp_selection`. These native
selection failures are checked explicitly. Visible-operation refusals retain their codes.

The lists fixture assigns explicit scopes to administrative operations and item ticking.
Its scripted grants exercise different scopes for the same subject. Its configuration
attaches the `lists.private` resource policy to `lists.get`, and the `items.tick` descriptor
names `items.private`; both admit only `owner` to the list `private`. Real OAuth introspection
coverage is in the integration tests. `scripted_access` is a fixture claim decoder; production
authentication uses verified tokens.

Tests hold and release handlers through explicit synchronisation. They do not rely on
elapsed wall-clock time. See the [lists executable](../../examples/lists/src/main.rs),
[conformance guide](../../docs/conformance.md) and [contract](../../docs/contract.md).
