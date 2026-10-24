# clapham-conformance

Reusable front-end conformance checks and a neutral lists-and-items contribution.

## Run a contribution through the harness

`run(&fixture, gate, front_ends)` takes a fresh contribution factory, explicit finite cases,
a scripted gate and front-end drivers. A schema cannot invent application identities or
initial state, so the caller supplies those cases. Each probe recreates the same fixture;
mutations in one front end cannot change another front end's expected result.

The harness checks every contributed operation has a schema-valid case. It compares each
front end with an independent `ComposedSet::call` baseline: `Outcome.result`, tool-error status,
refusal code/category/`retryable`, and CLI exit code. Records must arrive in order before
exactly one terminal result on the in-process and CLI front ends. Each namespace is also
composed twice and must refuse `namespace_claimed` before any driver starts.

The gate must admit built-in describe and refuse `fixture.denied_principal` for every domain
operation. The harness invokes those operations as that principal and compares describe's
admitted catalogue. Raw MCP drivers also compare every page of `tools/list` with describe,
rejecting leaked, missing or duplicate tools. Other selective denials can be supplied as
ordinary cases too. A hidden remote tool is absent at discovery, so the MCP client refuses
its binding as `mcp_selection`
before dispatch; that native selection refusal is checked explicitly. Refusals from admitted
operations keep their original code, category and `retryable` through every front end. Tool-error
outcomes are interpreted as refusals for comparison only when the baseline itself is a refusal;
the production client's outcome semantics are unchanged.

Use finite scripts. The lists suite uses a missing-list refusal for the indefinite watch and
uses finite export to prove ordered records. Existing watch tests separately exercise live
updates and cancellation with explicit holds. The harness introduces no sleep, timer, polling
loop or retry; hosts publish an HTTP readiness address after binding and shut down on stdin EOF.

```no_run
use clapham_conformance::{
    FrontEnd, InProcess, Process, ProcessFrontEnd, ProcessKind, lists_fixture, lists_gate, run,
};

# async fn check() -> Result<(), clapham_conformance::Failure> {
let fixture = lists_fixture();
let process = Process::new("path/to/lists");
let mut front_ends: Vec<Box<dyn FrontEnd>> = vec![Box::new(InProcess)];
for kind in [ProcessKind::CommandLine, ProcessKind::McpStdio, ProcessKind::McpHttp,
             ProcessKind::BoundStdio, ProcessKind::BoundHttp] {
    front_ends.push(Box::new(ProcessFrontEnd::new(kind, process.clone())));
}
let report = run(&fixture, lists_gate(), front_ends).await?;
assert_eq!(report.calls, fixture.cases.len() * 6);
# Ok(())
# }
```

For another contribution, replace `lists_fixture()` with
`Fixture::new(|| your_fresh_contributions(), vec![Case::new("items.get", input), ...])`.
Replace the gate with the same scripted policy your executable uses. `denied_principal`
defaults to `agent:denied` and can be changed. Include success, refusal and stream cases that
exercise the contribution's contract, including a retryable refusal where it has one. Drivers
use the application's real handlers; no call results are substituted by the harness.
`FrontEnd` is public so an application with different launch or authentication conventions
can implement its own driver. `Observation` retains ordered frames, catalogue names and
process exit status so the harness can check duplicate results and record ordering from
custom drivers.

Take `clapham` and `clapham-conformance` from the same released Git tag in another repository:

```toml
[dev-dependencies]
clapham = { git = "https://github.com/thirtytwobits/clapham", tag = "YOUR_RELEASE_TAG" }
clapham-conformance = { git = "https://github.com/thirtytwobits/clapham", tag = "YOUR_RELEASE_TAG" }
tokio = { version = "1", features = ["macros", "rt"] }
serde_json = "1"
```

Replace `YOUR_RELEASE_TAG` with a published tag containing the harness.
Rust 1.88 and Cargo are sufficient. In an application's Cargo integration test, obtain its
binary with `Process::new(env!("CARGO_BIN_EXE_your-binary"))` and await `run` in a Tokio test.
`cargo test -p clapham-lists --test conformance` demonstrates the complete process matrix.

### Reference process protocol

`Process` supplies a program, prefix arguments and environment without a shell. Drivers use:

- CLI: `--as KIND:ID <operation segments> --yes --format ndjson --json -`, with JSON on stdin;
- stdio: `serve --stdio --as KIND:ID`, with MCP requests and responses on the standard streams;
- HTTP: `serve --http 127.0.0.1:0`; the first stdout line is `{"address":"127.0.0.1:PORT"}`.
  Requests use `/mcp` and `Bearer KIND:ID`, preserving both parts of `Case::principal`.
  The host maps those credentials explicitly.
  Closing stdin requests graceful shutdown.

The process must seed the same state and use the same gate as the fixture. The reference
lists binary recognises the scripted `human:human`, `agent:agent`, `human:agent` and
`agent:denied` bearer credentials. Stdio notifications may precede a response or follow the
final result; they are ignored without accepting extra responses or mismatched request IDs.
Prefix arguments and environment can select another application's deterministic fixture. Drivers
reap successful child processes and kill their owned child on early failure.

`lists_fixture()` supplies a seeded Tasks list with two items and cases for every operation.
`lists_gate()` adds the fully denied caller to `ListsGate` while keeping describe available.
`lists.busy` provides a real retryable refusal: a double which drops `retryable` fails this
suite. Other doubles prove detection of changed codes/categories, wrong exit status, record
reordering, early results and duplicate results.

## Lists-and-items fixture

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
| `lists.busy` | `{}` | A declared `list_busy` refusal in `unavailable`, with `retryable: true` |
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
The rename handler's defensive numeric conversion uses `revision_out_of_range`
in `invalid_input`, distinct from the core dispatcher's `invalid_input` code.
The input schema rejects out-of-range revisions before dispatch; integral JSON
numbers such as `1.0` are accepted, and `u64::MAX` remains an exact integer fence.

`hold(name)` arms a one-shot `HeldCall`, or returns `None` for an unknown operation
or an already armed hold. Poll the call concurrently with `held.entered().await`;
then change state through the handle and call `held.release()`. Dropping the hold
also releases it, and an unentered dropped or released hold can be rearmed immediately.
Invalid input and denied admission do not consume holds. Watch
subscribes before its hold so changes made while held are queued; export snapshots
before its hold. Other handlers pause before reading or changing state. No state
lock spans a hold or a record sink acknowledgement.

Watch records carry `kind` and a complete resulting `list`; deleted records carry
the final snapshot. Each subscriber receives only its list's future changes in
revision order, with no initial replay. Watches follow a logical list ID through
deletion and recreation: `Deleted` is a record, and subsequent `Created` and other
records for the same ID arrive on the existing subscription. The stream remains open
until cancelled; revisions distinguish changes to successive lists with that ID and
prevent stale writes, while the subscription continues following the ID.
Sink acknowledgements supply backpressure.
Subscribers release their queues when cancelled. Finite scripts control fixture
state and queue sizes; connection budgets belong to the front ends. Cancellation
in flight reports `cancelled` with uncertain evidence through the core runner.
Streams propagate sink refusals unchanged; `record_delivery_failed` is declared
as unavailable, and other undeclared sink codes exercise the normal refusal policy.
