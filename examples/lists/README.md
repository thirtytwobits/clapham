# Lists example

A reference `main` over the neutral lists-and-items contribution. Every launch starts a fresh
Tasks list with two items; this example keeps its state in memory. The command line is built
from the descriptors and uses the process account as its default human principal.

```console
cargo run -p clapham-lists -- lists get tasks
cargo run -p clapham-lists -- lists export --id tasks --format ndjson
cargo run -p clapham-lists -- lists create --list-json '{"id":"new","name":"New"}'
cargo run -p clapham-lists -- lists compact --yes
cargo run -p clapham-lists -- --as agent:agent items tick --list-id tasks --id a
```

After installing or building the `lists` binary, use `lists <operation segments>` directly.
`lists describe`, descriptor help, confirmations, JSON stdin, formats and completions use the
same CLI library as an application. `agent:agent` follows the fixture's selective policy;
`agent:denied` is refused every domain operation and sees only the built-in catalogue.

```console
cargo run -p clapham-lists -- serve --stdio
cargo run -p clapham-lists -- serve --stdio --as agent:agent
cargo run -p clapham-lists -- serve --http 127.0.0.1:8080
```

Stdio takes the principal from launch configuration and reserves stdout for MCP. HTTP serves
`/mcp` and prints one JSON readiness line with its bound address. Its scripted authenticator
maps `Bearer human`, `Bearer agent` and `Bearer denied` to their fixed fixture principals;
unrecognised credentials refuse. This authenticator is restricted to a loopback listener.
The application owns the listener and shutdown: stdin closure or input, or Ctrl-C, ends the
HTTP server. Port zero chooses an ephemeral port for the harness without guessing or retries.

`main` shows composition, the shared gate, CLI dispatch, transport selection and per-request
authentication. Replace the in-memory fixture and scripted credentials with the application's
own contributions and authenticator when using it as a reference.

```console
cargo test -p clapham-lists --test conformance
```

The reusable harness launches this actual binary through all six front ends, checking results,
unchanged refusals and retryability, category exit codes, gate visibility and ordered records.
It runs in the workspace gates and the Rust 1.88 CI lane. See the
[conformance crate guide](../../crates/clapham-conformance/README.md) for downstream usage.
