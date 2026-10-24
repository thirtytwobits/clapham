# Running the conformance harness

Use `clapham_conformance::run(&fixture, gate, front_ends)` in a Cargo integration test in the
application that owns the executable. The [contract](contract.md#c12-one-fixture-can-be-checked-through-every-front-end)
records the guarantees and tests. The [crate guide](../crates/clapham-conformance/README.md)
contains the complete API example and process protocol; [release usage](using-a-release.md)
shows the dev-dependencies by tag.

## Start with the reference application

```console
cargo test -p clapham-lists --test conformance
```

[The test](../examples/lists/tests/conformance.rs) obtains the executable through Cargo's
`CARGO_BIN_EXE_lists`, seeds a fresh fixture and selects these drivers:

| Driver | Invocation | Records checked |
|---|---|---|
| `InProcess` | `ComposedSet::call` against fresh contributions | Yes |
| `ProcessKind::CommandLine` | Actual CLI child process | Yes |
| `ProcessKind::McpStdio` | Actual MCP stdio server | No; final result only |
| `ProcessKind::McpHttp` | Actual authenticated HTTP server | No; final result only |
| `ProcessKind::BoundStdio` | MCP client binds the stdio server as a contribution | No |
| `ProcessKind::BoundHttp` | MCP client binds the HTTP server as a contribution | No |

A separate in-process invocation supplies the expected result for each driver. The reference
suite checks 18 explicit cases and 12 denied-operation probes across all six drivers: 180
call and gate comparisons, plus two namespace collision checks. The base `lists_fixture()`
has 17 cases; the executable test adds the human counterpart to its agent identity case.

## Supply another contribution

1. Build `Fixture::new(factory, cases)`. The factory returns fresh contributions and the same
   initial state on every invocation. Put at least one schema-valid, finite `Case` in the
   vector for every contributed operation. Add success, refusal, retryable-refusal and
   ordered-stream cases appropriate to the operation's actual contract.
2. Supply the same scripted gate to `run` and the executable. `fixture.denied_principal`
   defaults to `agent:denied`; it must be refused every domain operation while describe stays
   available. Supply matching `fixture.bounds` and host bounds.
3. Set `Process::new(env!("CARGO_BIN_EXE_your-binary"))`, then choose `ProcessFrontEnd` drivers.
   The binary follows the documented CLI/serve protocol, or the application implements
   `FrontEnd` for different launch or authentication conventions. Prefix arguments and
   environment can select deterministic fixture state.
4. Await `run` in a Tokio test and propagate `Failure`. A successful `Report` counts completed
   comparisons; errors identify the driver, operation and mismatch. Every namespace is also
   composed twice and must fail `namespace_claimed` before any driver starts.

The CLI driver sends JSON on stdin, including inputs beyond Windows' argv limit. The HTTP
host announces its bound loopback address instead of being polled; credentials preserve both
principal kind and identity. Stdio takes identity from launch configuration. EOF requests
server shutdown, and drivers reap their successful children or kill owned children on failure.
The fixture and executable must agree on each caller's authentication mapping.

## Interpret a result

The harness compares `Outcome.result` and tool-error status, native refusal code/category/
retryability, CLI exit code, gate-filtered describe catalogues and ordered record frames.
Raw MCP drivers also compare every `tools/list` page. Record-bearing observations must end
with exactly one terminal result after the records. A hidden remote tool cannot be bound:
the client must produce native `mcp_selection` before dispatch, rather than the server's
`not_admitted`. This exception is explicit; admitted operation refusals still retain their
original fields. A structured refusal in a remote tool-error outcome is interpreted only for
comparison against a native refusal baseline; production client outcomes are unchanged.

Use finite cases. The lists watch case refuses a missing list; live watch records and
cancellation are checked separately with holds. An indefinite watch against an existing list
would not finish this finite matrix. Custom drivers must explicitly coordinate cancellation
if they cover such a stream. The harness supplies no timeout, sleep, polling or retry loop.

## Fixture and adapter evidence

`clapham-conformance` provides the neutral contribution that front ends and other
contributions are tested beside. The fixture has no disk state or runtime requirement;
the process and transport harness uses Tokio.
See the [crate guide](../crates/clapham-conformance/README.md) for operation inputs,
outputs and a complete composition example.

`ListsHandle` shares state across the two namespaces and any composed sets made
from them. `ListsGate` admits descriptor defaults while denying agents `items.tick`.
The contribution covers all four effect classes, positional and object inputs,
resources, preview, a revision fence, finite export, an indefinite watch and an
undeclared refusal.

The tests call every operation through `ComposedSet::call`. Holds, sink
acknowledgements and change notifications use explicit futures. A watch test
subscribes before holding, changes the list while held, then proves live delivery
and cancellation on the ordinary executor. Watches follow the logical list ID through
deletion and recreation until cancelled; a regression checks the ordered `Deleted`,
`Created` and subsequent change records. A fenced rename is held while a scripted
write advances the revision, then refuses without overwriting the newer state.
A preview returns the real create result shape without storing a list or consuming
a revision. Export retains its initial snapshot while a sink acknowledgement is held.

`cargo test --locked -p clapham-conformance` runs these checks. They are also part of
`cargo xtask check` on both CI platforms and the Rust 1.88 minimum-version lane.
The CLI tests drive the generated command tree in-process: positional reads,
object flags, whole-object JSON from arguments and stdin, descriptor help,
principal overrides, admission and validation before dispatch. Process tests use
the `clapham-cli-fixture` binary to verify output, exit codes, effect safeguards,
completions and a deadline signalled after a write is explicitly held. Terminal
status is supplied by the fixture for deterministic human-mode checks. The reusable harness
drives all front ends against fresh copies of the same fixture.

The MCP tests drive a real child process over stdin/stdout and deterministic duplex streams.
They cover effect annotations, principal filtering, unchanged refusals, preview isolation,
resource binding, finite-stream draining, held-call cancellation, selection, pagination and
encoded message limits. No elapsed-time checks are used.

The stateless HTTP harness uses scripted credentials, reuses one service across human and
agent requests, and holds explicit futures to check concurrent identity, cancellation and
queue bounds. Unmapped credentials are rejected before body consumption or gate admission.
`crates/clapham-conformance/records/mcp-protocol.json` records the lists wire surface;
`cargo test -p clapham-conformance --test mcp_protocol` refuses stale output. To write an
intentional update, run that test with `CLAPHAM_WRITE_RECORDS=1` and review the generated diff.
The test compares canonical bytes and takes request metadata keys from the server's published
`REQUEST_META_KEYS`, rather than the HTTP fixture. `cargo xtask check` removes the writing flag
from child Cargo commands, even if it was inherited from the caller.

The MCP client conformance tests bind the server over stdio and authenticated Streamable HTTP.
A composed `remote.lists.get` returns the same structured result as its local counterpart.
A remote `list_not_found` remains a tool-error outcome with the refusal code unchanged.
Scripted in-flight cancellation preserves uncertain dispatch evidence.

The public `run(&fixture, gate, front_ends)` API compares explicit finite cases against an
independent in-process baseline. The reference lists executable is launched for the CLI,
stdio server, HTTP server and the MCP client bound to both servers. CLI inputs travel on
stdin, including a Unicode JSON case larger than Windows' argument limit. HTTP readiness
is an explicit bound-address line, and stdin EOF shuts servers down without timed polling.

Every operation has a case, and a fully denied principal is checked for refusal and omission
from describe. The raw MCP drivers also compare the complete paginated `tools/list` catalogue.
The selective agent denial for `items.tick` is checked too, alongside a human with the same
identity, proving HTTP preserves the principal's kind. A hidden tool
cannot be bound by the client, so that front end must refuse as `mcp_selection`; admitted
operation refusals retain code, category and retryability through all adapters. Finite export
checks records before exactly one result on the two front ends that carry records. The
watch uses a missing-list refusal in this finite matrix; its live stream and cancellation
remain covered by the scripted watch tests above. Duplicate namespace admission is checked
before a driver starts.

`lists.busy` returns a declared retryable refusal, so a front-end double which drops
`retryable` is rejected. Other doubles exercise code/category loss, exit status, ordering and
duplicate results. The crate README documents downstream Git-tag consumption and custom
`FrontEnd` drivers. `cargo test -p clapham-lists --test conformance` runs the complete matrix;
workspace checks on both CI platforms and the Rust 1.88 lane include it.
