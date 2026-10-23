# clapham

The contract for operations, descriptors, refusals, principals, composition, selection,
gates and calls.

`Operation` is the serialisable descriptor. Its name uses 1 to 128 ASCII letters,
digits, `_`, `-` and `.`, and splits at the first dot. `description` is required;
`input` declares an object root. `output` is an optional JSON Schema. Omitted
`principals` defaults from the effect; an explicit empty list admits nobody.
`idempotent` and `open_world` are explicit, required hints.

`Effect::annotations` maps the class and those hints to MCP. Queries are read-only
and idempotent; mutations and administrative operations are non-destructive;
destructive operations carry the destructive hint. An administrative class is
recorded under `io.github.thirtytwobits.clapham/effect` in the tool's `_meta`.
The returned `Annotations::meta` belongs on the tool itself, alongside its MCP
annotations. `Operation::annotations` supplies its declared hints to this mapping.

`Principal` carries `id` and `kind`. `Refusal` preserves the contributor's `code`,
with a fixed `Category`, message, optional JSON details and retryability flag.
Its `evidence` distinguishes `not_dispatched`, `completed` and `uncertain` effects.
`Category::exit_code` supplies the command-line exit code. A new handler refusal
defaults to completed evidence; pre-dispatch checks use `Refusal::not_dispatched`.

`Contribution::new(namespace).operation(descriptor, handler)` pairs descriptors
with object-safe asynchronous handlers or closures. `compose(contributions, bounds)`
checks ownership, descriptions and finite budgets, then compiles the input and
output schemas once. It refuses unresolved external references without fetching
them. A namespace is one name segment; each operation has that namespace before
its first dot. `clapham` is reserved for the built-in contribution.

`ComposedSet` is immutable and cheaply clonable. `operations` lists descriptors in
name order, `get` returns a compiled operation, and `revision` digests canonical
descriptors in name order. Nested JSON keys are sorted too, so revision does not
depend on insertion order or the process.

`set.select(names)` selects complete operation names; `set.all()` selects every
operation. A selection belongs to its originating set and its clones. Its revision
digests the selected descriptors and any remote revisions supplied through
`Contribution::with_binding_revision`, so approvals can be tied to that selection.

`ComposedSet::call` first returns any cancellation already signalled on entry.
It then checks selection membership, validates input, asks the gate,
checks preview support and locality, runs the handler, then validates successful
output. Cancellation is checked again before dispatch. `DefaultGate` admits the
descriptor's principal kinds. Use
`compose_with_options` to supply a custom `Gate` and `ExecutionMode::Daemon` where
daemon-local operations can run. A gate may allow, deny or leave approval pending;
pending calls never dispatch. The default execution mode is `InProcess`.

Declared handler refusals retain their code, details, retryability and evidence;
their category comes from the descriptor. An undeclared code becomes
`undeclared_refusal`, with the original refusal in its details. Runner refusals
are independent of the handler's declared set.

`ComposedOperation::invoke` remains the low-level dispatch primitive. It validates
input and successful output and handles cancellation; it does not enforce selection,
gate, preview, locality or declared-refusal policies. Use `ComposedSet::call` to
apply those policies. Low-level `clapham.describe` uses the full catalogue, filtered
by the gate.

Every set includes `clapham.describe` and `clapham.version`. The former lists the
caller's selected, gate-admitted descriptors in name order, including their schemas.
Its optional `name` input returns one descriptor. Unknown, unselected, denied and
pending names all refuse as `unknown_operation`. The latter returns the library's
version and any application name and version supplied in `ComposeOptions::application`.
Both are workspace queries and consume the same composition budgets as contributed
operations. A selection must include a built-in operation to call it.

`Call` carries the principal, input, asynchronous `RecordSink`, `Cancellation` and
preview flag. A sink's acknowledgement provides backpressure. Cancellation before
dispatch yields not-dispatched evidence; cancellation during the handler yields
uncertain evidence. A returned handler result or refusal retains its evidence.

`Outcome` preserves structured JSON, optional ordered content, the tool-reported
error flag and evidence. Absent structured output differs from a JSON null. Content
blocks retain text, image and audio references, resource links and embedded text or
base64 resources in order. Successful output is schema-validated; a tool-reported
error carries its error shape unchanged.

The published schemas are [operation.schema.json](schemas/operation.schema.json)
and [refusal.schema.json](schemas/refusal.schema.json), alongside the describe result's
[catalogue.schema.json](schemas/catalogue.schema.json). Generate them with
`cargo xtask schemas`; `cargo xtask schemas --check` refuses stale output and runs
inside `cargo xtask check`.
