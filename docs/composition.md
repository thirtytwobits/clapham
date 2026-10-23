# Composition and dispatch primitives

Contributors claim one namespace with `Contribution::new(namespace)` and add
descriptors and handlers with `.operation(descriptor, handler)`. A handler can
implement the object-safe `Handler` trait or be an asynchronous closure. Its boxed
future may borrow the handler, allowing a contribution to retain its own state.

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
ends will share, including the combined 64 MiB buffering limit.

`revision()` is `sha256:` followed by a lowercase hexadecimal SHA-256 digest. Its
preimage is the UTF-8 prefix `clapham.composed-set.v1` followed by a zero byte, then
the compact JSON array of descriptors in complete-name order. Every JSON object's
keys are recursively sorted; array order is retained. Handlers and bounds do not
participate. Changing a descriptor changes its revision; changing construction order
does not. Tests compare revisions across separate processes and map backends.

`ComposedOperation::invoke` prepares input and the operation's own name, dispatches
the handler, and checks successful output. It is the primitive the selection and
admission runner will use. Hosts apply admission, preview and declared-refusal
policies before exposing calls to users; those policies belong to issue #6.

`Refusal::new` and `Outcome::new` initially carry `completed` evidence. Checks before
dispatch set `not_dispatched`. Cancelling a handler in flight yields `uncertain`;
a returned remote refusal or outcome keeps its explicit evidence. Completion does
not claim durable publication. The refusal schema now requires evidence, so a caller
cannot silently treat omitted evidence as proof that nothing ran.

Records flow through the supplied asynchronous sink, whose acknowledgement supplies
backpressure. Cancellation uses a clonable signal without requiring a timer or a
particular async runtime. Tests hold handlers and sinks with explicit release signals.
Ordered content preserves all five block kinds and distinguishes absent structured
output from explicit JSON null. A tool-reported error remains an outcome with
`is_error`, rather than becoming a dispatch refusal or being checked as successful output.
