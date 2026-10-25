# Lists example

A reference application loading [JSON configuration](configuration.json), registering the
lists/items providers and adding an operation with a typed Rust builder before composition.
It starts a Tasks list with two items and keeps server state in memory.

The executable serves MCP and the operation HTTP interface through one OAuth authenticator.
Configure the introspection endpoint and resource-server credentials as described in
[the application guide](../../docs/composing.md). Set `CLAPHAM_ENDPOINT` and an
appropriate `CLAPHAM_ACCESS_TOKEN` for the CLI.

```console
cargo run -p clapham-lists -- serve --http 127.0.0.1:8080
cargo run -p clapham-lists -- lists get tasks
cargo run -p clapham-lists -- lists export --id tasks
```

Stdio verifies the launch token before binding authority to its private connection.
The application owns listening and shutdown; HTTP prints a readiness address and stops on
stdin closure or Ctrl-C. The CLI calls the service and preserves ordered record delivery.

The conformance harness selects fixture mode through the trusted host environment. It supplies
scripted claim snapshots for testing instead of contacting an authorisation server. Fixture
mode is restricted to loopback listeners. The six-front-end test checks different grants for
the same subject, visibility, results, refusals, exit codes and ordered records.

```console
cargo test -p clapham-lists --test conformance
```

For the minimal typed operation, see [the typed example](../typed/src/main.rs).
