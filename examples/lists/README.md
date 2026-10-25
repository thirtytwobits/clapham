# Lists example

A reference application loading [JSON configuration](configuration.json), registering the
lists/items providers and adding an operation with a typed Rust builder before composition.
It starts a Tasks list with two items and keeps server state in memory.

The executable serves MCP through one OAuth authenticator, and its CLI calls that server
through Clapham's MCP client. Configure the introspection endpoint and resource-server
credentials as described in [the application guide](../../docs/composing.md). Set
`CLAPHAM_ENDPOINT` to the MCP endpoint and an appropriate `CLAPHAM_ACCESS_TOKEN` for the CLI.

```console
cargo run -p clapham-lists -- serve --http 127.0.0.1:8080
CLAPHAM_ENDPOINT=http://127.0.0.1:8080/mcp cargo run -p clapham-lists -- lists get tasks
CLAPHAM_ENDPOINT=http://127.0.0.1:8080/mcp cargo run -p clapham-lists -- lists export --id tasks
```

With `CLAPHAM_TRANSPORT=stdio` the CLI launches `serve --stdio` and calls it over its standard
streams. Stdio verifies the launch token before binding authority to its private connection.
The application owns listening and shutdown; HTTP prints a readiness address and stops on
stdin closure or Ctrl-C. The CLI preserves ordered record delivery.

The conformance harness selects fixture mode through the trusted host environment. It supplies
scripted claim snapshots for testing instead of contacting an authorisation server. Fixture
mode is restricted to loopback listeners. The seven-front-end test checks different grants for
the same subject, visibility, results, refusals, exit codes and ordered records.

```console
cargo test -p clapham-lists --test conformance
```

For the minimal typed operation, see [the typed example](../typed/src/main.rs).
