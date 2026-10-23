# Composition and dispatch primitives

Contributors claim one namespace with `Contribution::new(namespace)` and add
descriptors and handlers with `.operation(descriptor, handler)`. A handler can
implement the object-safe `Handler` trait or be an asynchronous closure. Its boxed
future may borrow the handler, allowing a contribution to retain its own state.
Use `.operation_boxed(descriptor, handler)` to retain an existing handler box directly.

`compose(contributions, bounds)` consumes the contributions and produces an immutable
`ComposedSet`. Cloning the set shares the compiled operations. Its public accessors
expose immutable descriptors and validators, with no registration or mutation after
composition.

Each namespace is a non-empty segment of at most 128 ASCII letters, digits, `_` and
`-`. Each complete operation name must belong to that namespace and have a non-empty
local name. `OperationName` already refuses characters outside the complete-name
alphabet before a descriptor can be built. Namespaces are not aliased. The London
Project reserves `clapham`, `clerkenwell`, `walthamstow` and `battersea`;
composition enforces `clapham`, whose built-in contribution is added in its own issue.

Composition refuses repeated namespaces as `namespace_claimed`, invalid names or
ownership as `invalid_name`, empty or whitespace descriptions as `missing_description`,
invalid schemas as `invalid_schema`, and an oversized input or output schema as
`schema_too_large`. Duplicate complete names are `duplicate_operation`; invalid bounds,
catalogue byte overflow and operation count overflow have `invalid_bounds`,
`catalogue_too_large` and `too_many_operations` codes respectively.

Schemas compile once with local references enabled. Composition refuses references
that would require external retrieval. Schema sizes count compact serialised UTF-8
bytes, separately for input and output. The catalogue limit counts the compact JSON
array of descriptors, including separators. It is charged before retaining each
operation's validators. `Bounds` also carries the connection budgets the MCP front
ends will share, including the combined 64 MiB buffering limit. The catalogue byte
limit starts at 2, enough for the empty array `[]`.

Outputs may be any JSON shape. MCP 2026-07-28 explicitly permits arrays and scalar
structured results; see its [tool result specification](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/server/tools.mdx#structured-content).
The core also accepts boolean JSON Schemas. MCP's `outputSchema` field uses an object
representation, so the adapter should encode `true` as `{}` and `false` as
`{"not": {}}` at that seam rather than restrict the core's result shapes.

An explicit empty `principals` list admits nobody and must not regain effect defaults.
Positional bindings are CLI metadata. Validation of supported bindings belongs to CLI
command-tree construction (issue #9) and must account for schema references and
composition rather than assume every property appears in the root `properties` map.

`revision()` is `sha256:` followed by a lowercase hexadecimal SHA-256 digest. Its
preimage is the UTF-8 prefix `clapham.composed-set.v1` followed by a zero byte, then
the compact JSON array of descriptors in complete-name order. Every JSON object's
keys are recursively sorted; array order is retained. Handlers and bounds do not
participate. Changing a descriptor changes its revision; changing construction order
does not. Tests compare revisions across separate processes and map backends.
Both map backends also assert the same fixed v1 digest for an unordered fixture.
Framing or canonicalisation changes need a new version prefix. Intentional changes to
the descriptor wire form or its defaults require review of the fixture and its digest.
JSON numbers retain their serialised representation: `1` and `1.0` produce different
revisions; this is key ordering, not numeric or semantic normalisation.

`ComposedOperation::invoke` prepares input and the operation's own name, dispatches
the handler, and checks successful output. It is the primitive the selection and
admission runner will use. Hosts apply admission, preview and declared-refusal
policies before exposing calls to users; those policies belong to issue #6.

`Refusal::new` and `Outcome::new` initially carry `completed` evidence. Checks before
dispatch use `Refusal::not_dispatched`. Cancelling a handler in flight yields `uncertain`;
a returned remote refusal or outcome keeps its explicit evidence. Completion does
not claim atomic effects or durable publication. When cancellation and completion are
both ready at a poll, cancellation takes precedence and yields uncertain evidence.
The refusal schema now requires evidence, so a caller
cannot silently treat omitted evidence as proof that nothing ran.

Records flow through the supplied asynchronous sink, whose acknowledgement supplies
backpressure. Cancellation uses a clonable signal without requiring a timer or a
particular async runtime. Tests hold handlers and sinks with explicit release signals.
Sink refusals propagate their evidence unchanged. A delivery failure that leaves the
operation's effects unresolved must carry `uncertain` evidence, even when the handler
propagates it with `?`.
Ordered content preserves all five block kinds and distinguishes absent structured
output from explicit JSON null. A tool-reported error remains an outcome with
`is_error`, rather than becoming a dispatch refusal or being checked as successful output.
