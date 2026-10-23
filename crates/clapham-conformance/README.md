# clapham-conformance

The neutral lists-and-items contribution used to prove calls through each front end.
The front-end harness follows with the adapters; this crate supplies the fixture now.

`ListsHandle::new()` creates shared empty in-memory state. `contributions()` returns
the `lists` and `items` namespaces over that state. Repeated calls return fresh
handlers sharing the same fixture, so independent composed sets can run beside one
another. `ListsGate` follows descriptor defaults and additionally denies agents
`items.tick`, demonstrating a refused mutation.

```rust
use std::sync::Arc;
use clapham::{Bounds, Cancellation, ComposeOptions, Principal, PrincipalKind, RecordSink};
use clapham_conformance::{ListsGate, ListsHandle};
use serde_json::json;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let lists = ListsHandle::new();
let set = clapham::compose_with_options(lists.contributions(), Bounds::default(), ComposeOptions {
    gate: Arc::new(ListsGate),
    ..Default::default()
})?;
let created = futures_executor::block_on(set.call(
    &set.all(), Principal::new("list-reader", PrincipalKind::Human), "lists.create",
    json!({"list": {"id": "first", "name": "First"}}),
    RecordSink::default(), Cancellation::new(), false,
))?;
assert_eq!(created.result.unwrap()["id"], "first");
assert!(lists.get("first").is_some());
# Ok(())
# }
```

| Operation | Input | Result or records |
|---|---|---|
| `lists.list` | `{}` | List snapshots in identity order; resource `lists://` |
| `lists.get` | `id` | One list; positional `id`, resource `lists://{id}` |
| `lists.create` | `list: {id, name}` | New list; preview returns the same shape without committing |
| `lists.rename` | `id`, `name`, `revision` | Renamed list, fenced against the supplied revision |
| `lists.compact` | `{}` | `{"removed": count}` after removing ticked items; administrative |
| `lists.delete` | `id` | Final deleted snapshot; destructive |
| `lists.watch` | `id` | Future `ListChange` records until cancelled |
| `lists.export` | `id` | Snapshot items as records, then `{"count": count}` |
| `lists.fault` | `{}` | An intentional undeclared `fixture_fault` refusal |
| `items.add` | `list_id`, `item: {id, text}` | Updated list with a new unticked item |
| `items.tick` | `list_id`, `id` | Updated list; an already ticked item leaves the revision unchanged |

Input objects reject extra properties. Identities and list names are non-empty;
item text may be empty. Lists contain `id`, `name`, `revision` and insertion-ordered
`items`; items contain `id`, `text` and `ticked`. One global revision sequence
prevents an old fence matching a deleted and recreated list. `list_changed` carries
the current revision in `details.revision`. Missing lists use `list_not_found`;
duplicate lists or items and missing items have declared refusals. Scripted writes
use `create_list`, `rename_list` and `add_item`; `get` and `snapshot` return detached
copies for assertions.

`hold(name)` arms a one-shot `HeldCall`, or returns `None` for an unknown operation
or an already armed hold. Poll the call concurrently with `held.entered().await`;
then change state through the handle and call `held.release()`. Dropping the hold
also releases it. Invalid input and denied admission do not consume holds. Watch
subscribes before its hold so changes made while held are queued; export snapshots
before its hold. Other handlers pause before reading or changing state. No state
lock spans a hold or a record sink acknowledgement.

Watch records carry `kind` and a complete resulting `list`; deleted records carry
the final snapshot. Each subscriber receives only its list's future changes in
revision order, with no initial replay. Sink acknowledgements supply backpressure.
Subscribers release their queues when cancelled. Finite scripts control fixture
state and queue sizes; connection budgets belong to the front ends. Cancellation
in flight reports `cancelled` with uncertain evidence through the core runner.
Streams propagate sink refusals unchanged; `record_delivery_failed` is declared
as unavailable, and other undeclared sink codes exercise the normal refusal policy.
