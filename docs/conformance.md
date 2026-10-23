# Lists-and-items fixture

`clapham-conformance` provides the neutral contribution that front ends and other
contributions are tested beside. It has no disk state or runtime requirement.
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
status is supplied by the fixture for deterministic human-mode checks. The remaining
front-end conformance harness follows when those adapters are implemented.

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
