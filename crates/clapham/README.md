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
defaults to completed evidence; pre-dispatch checks set not-dispatched evidence.

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

`ComposedOperation::invoke` is the low-level dispatch primitive: it validates input,
sets `Call::name` to the operation's own name, runs the handler and validates
successful output. The host applies selection, admission, preview support and
declared-refusal policies around dispatch. Those policies follow with the call
runner. The built-in operations follow with their own contribution.

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
and [refusal.schema.json](schemas/refusal.schema.json). Generate them with
`cargo xtask schemas`; `cargo xtask schemas --check` refuses stale output and runs
inside `cargo xtask check`.
