# Composing an application

The application assembles Clapham in `main`. A piece supplies a `Contribution` containing its
own descriptors, handlers and state; it does not create the application's command line,
listener or policy. Use the [contract](contract.md) for guarantees and
[composition reference](composition.md) for name, schema, budget and revision rules.

## From a contribution to a call

This complete example creates one query, composes it, selects it alongside discovery and
calls it as a human. It needs `clapham`, `serde_json` and `futures-executor` in Cargo:

```rust
use clapham::{
    Bounds, Call, Cancellation, Contribution, Operation, Outcome, Principal, PrincipalKind,
    RecordSink, compose,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let descriptor: Operation = serde_json::from_value(json!({
        "name": "lists.count",
        "description": "Count the lists in this workspace.",
        "input": {"type": "object", "additionalProperties": false},
        "output": {"type": "integer", "minimum": 0},
        "effect": "query", "locality": "workspace",
        "idempotent": true, "open_world": false
    }))?;
    let contribution = Contribution::new("lists").operation(descriptor, |_: Call| async {
        Ok(Outcome::new(json!(2)))
    });
    let set = compose([contribution], Bounds::default())?;
    let selection = set.select(["lists.count", "clapham.describe"])?;
    let result = futures_executor::block_on(set.call(
        &selection,
        Principal::new("operator", PrincipalKind::Human),
        "lists.count",
        json!({}),
        RecordSink::default(),
        Cancellation::new(),
        false,
    ))?;
    assert_eq!(result.result, Some(json!(2)));
    Ok(())
}
```

The constant result stands in for the piece's own state. A real handler can capture an
`Arc` or implement the object-safe `Handler` trait and borrow retained state. It receives
the authenticated caller and its own complete operation name. Call the composed set,
rather than invoking a handler directly, to enforce selection, validation, gate, preview,
locality and the declared refusal set.

## Install the application's gate

`compose` selects `DefaultGate` and in-process execution. With `compose_with_options`, put an
`Arc<dyn Gate>` in `ComposeOptions::gate`. The gate receives the principal and descriptor and
returns `Allow`, `Deny` or `Pending`. It may read synchronised host policy state. An explicit
empty descriptor principal list stays empty; a custom gate deliberately overriding defaults
owns that decision. The same gate controls describe visibility, so its query must be free of
approval side effects. Handle an actual `admission_pending` call at the application boundary.

Set `ComposeOptions::application` to an `ApplicationInfo` to include application name/version
in `clapham.version`. Set `execution: ExecutionMode::Daemon` only in a host that provides the
daemon lifecycle. It enables daemon-local calls; it does not start a daemon or acquire a lock.
Selections restrict the exposed operation names and carry revisions, not independent rights.

## The lists application

[`examples/lists/src/main.rs`](../examples/lists/src/main.rs) is the complete executable.
It composes fresh `lists` and `items` contributions from `lists_fixture()`, installs
`lists_gate()` and selects its front end from the process arguments:

| Invocation | Assembly and ownership |
|---|---|
| `lists <operation segments>` | `Cli::new("lists", set, process_principal()?)` then `run().await`; return its exit code |
| `lists serve --stdio` | `Server::new(set, launch_principal)` then `serve_stdio`; await transport completion |
| `lists serve --http ADDR` | `HttpServer::new(set, authenticator)` mounted at `/mcp`; application binds listener and handles shutdown |

```console
cargo run -p clapham-lists -- lists get tasks
cargo run -p clapham-lists -- lists export --id tasks --format ndjson
cargo run -p clapham-lists -- --as agent:agent items tick --list_id tasks --id a
cargo run -p clapham-lists -- serve --stdio --as agent:agent
cargo run -p clapham-lists -- serve --http 127.0.0.1:8080
```

The agent's tick is refused by the gate. Every launch seeds a fresh Tasks list with two
items, so one command's mutation is not persistent across launches. The example uses only
workspace operations; it is a reference for wiring, not a workspace store or lock service.
Its HTTP authenticator has fixed loopback-only credentials, not a production identity system.
See [the example guide](../examples/lists/README.md) for readiness, credentials and shutdown.

For an application, acquire workspace exclusivity before composing handlers over the held
workspace. Retain that ownership until calls finish. Supply real authentication and policy,
own process reaping/listeners/TLS and coordinate shutdown. CLI 0.1.0 refuses daemon-local
operations; it does not discover or connect to a running daemon.

## Add remote operations

Create an MCP connection with a host-approved endpoint or process streams, a namespace,
credential-scope revision and bounds. Discover and bind selected names, then add
`binding.contribution()` beside the local contributions. Use daemon execution mode for these
daemon-local operations. Retain or explicitly close the connection, and reap any child
process after its shutdown signal. [The client guide](mcp.md#client) covers aliases, principal
authority, stale bindings and uncertain effects. A namespace collision still fails ordinary
composition before any front end can start.

## Check the assembled front ends

Supply a fresh fixture factory and finite cases to the [conformance harness](conformance.md).
The executable and in-process fixture must use the same initial state, bounds and scripted
gate. The [lists integration test](../examples/lists/tests/conformance.rs) runs all six
drivers, including a human and an agent sharing one identity, against real child processes.
Application stores and approval workflows stay outside the reusable fixture library.
