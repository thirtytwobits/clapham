# clapham

Transport-independent operations, typed Rust handlers, shared JSON/Rust configuration,
OAuth authority, resource policies, validation, refusals and ordered records. Requires Rust 1.88.

## Quickstart

Define and compose a typed query. Schemas and wire conversion are generated from Rust types.

```rust
use clapham::{
    Bounds, Call, Configuration, Contribution, Effect, Exposure, TypedOperation,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CountInput {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = TypedOperation::<CountInput, u64>::new(
        "lists.count",
        "Count the lists in this workspace.",
        Effect::Query,
    )?.require_scope("lists:read");
    let lists = Contribution::new("lists")
        .typed_operation(count.clone(), |_: Call, _: CountInput| async { Ok(2) });
    let set = Configuration::new(
        "https://identity.example.com",
        "https://lists.example.com",
    )
        .provider("lists", lists)
        .expose(Exposure::typed("lists", &count))
        .compose(Bounds::default())?;
    assert!(set.get("lists.count").is_some());
    Ok(())
}
```

The [typed executable](../../examples/typed/src/main.rs) serves this query through authenticated
MCP and operation HTTP, and supplies its generated CLI. It uses `clapham-cli`,
`clapham-mcp-server`, `serde`, `schemars`, Tokio and axum. With an OAuth authorisation server
configured as described in the [application guide](../../docs/composing.md), start the server
and call the generated command:

```console
cargo run -p clapham-typed -- serve
cargo run -p clapham-typed -- lists count
```

## Operations and composition

[`TypedOperation`] defines an input type and output type. [`Contribution::typed_operation`]
registers an asynchronous handler receiving [`Call`] and typed input. Use
[`Contribution::typed_operation_with_outcome`] for explicit effect evidence. Input schemas use
Serde's deserialisation representation; output schemas use its serialisation representation.
The shared runner validates schemas and Rust conversion before handlers run.

[`Configuration`] accepts JSON or typed Rust builders. Both resolve registered providers,
connections and resource policies through the same atomic composition path. Load JSON with
[`Configuration::from_json`], register application code and extend it with builders before
composition. Generated schemas belong to providers; configuration selects operations and
adds restrictions. Namespace claims, duplicate registrations, aliases, schemas, typed policy
compatibility and budgets are checked before a set is exposed.

[`Operation`] remains the language-neutral descriptor used by adapters and dynamic providers.
[`compose`] and [`compose_with_options`] accept trusted host registrations directly.
Their optional OAuth resource binding serves trusted hosts that establish issuer/audience
elsewhere; HTTP servers require an explicit binding.
[`ComposeOptions::policies`] resolves intrinsic resource-policy names through the same checks.

## Authority and execution

[`OAuthContext`] carries authority established by trusted OAuth verification or introspection.
Its issuer, audience, subject, client, scopes and extension claims are available to application
policy. External interfaces authenticate requests before constructing this context. A stdio
host binds verified authority to its privately launched connection.

Every declared scope is mandatory, independently of effect hints or a custom [`Gate`].
[`ResourcePolicy`] checks verified authority against typed input before the operation handler
runs. Resource-policy refusal codes must be declared by the operation. Composition preserves provider requirements and adds configured restrictions. Discovery
checks operation-level authority; resource-specific access is checked on invocation.

[`ComposedSet::call`] enforces selection, OAuth resource binding, scopes, input validation,
host admission, preview, locality, resource policies, cancellation and output validation.
Selections and canonical revisions identify definitions; permissions are checked on each call.

A declared refusal retains its code, details, retryability and evidence; its category comes
from the descriptor. Undeclared handler codes become `undeclared_refusal`; a record-sink
refusal the handler returns is carried unchanged. Dispatcher conversion failures remain
`invalid_input` or `invalid_result`. Evidence distinguishes
`not_dispatched`, `completed` and `uncertain`; completed does not imply durable effects.

[`RecordSink`] acknowledges each record before the producer continues. [`Outcome`] carries
structured output, opaque adapter content, an error flag and evidence. Adapters own native
content types. Absent results differ from JSON null; absent content differs from empty content.

The [contract](../../docs/contract.md) links guarantees to executable evidence. Published
operation, refusal, catalogue and configuration schemas are generated and checked by
`cargo xtask schemas`. Public Rust exports are recorded by `cargo xtask api`.
