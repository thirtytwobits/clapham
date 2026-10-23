# Selections, admission and calls

`set.select(["lists.read", "lists.append"])` creates an immutable selection of
complete operation names. Unknown names refuse as `unknown_operation`; repeated
names are deduplicated. `set.all()` selects every operation. Both expose
descriptors in name order and a stable `revision()`.

A selection belongs to the set that created it and its clones. An independently
composed set cannot use it, even when its descriptors and revision match. A revision
identifies selected contract data; it is not an admission decision or a capability
to dispatch against another set.

## Revisions for approval

Selections digest selected descriptors in complete-name order. A contribution may
add an opaque remote binding revision with `.with_binding_revision(revision)`.
That revision participates only when the selection includes an operation from
that contribution. The host must change it when the remote binding changes.
Handlers, bounds, gates, execution modes and principals do not participate.

Without remote bindings, the preimage and canonicalisation match the
[composed-set revision](composition.md): an all-operation selection has the set's
revision. With remote bindings, the preimage is `clapham.selection.v1`, a zero byte,
then compact JSON with `operations` (the selected descriptor array) and `bindings`
(an object mapping each selected remote namespace to its revision). Object keys
are recursively sorted and array order is retained. SHA-256 produces the same
`sha256:` lowercase hexadecimal form. Fixed fixtures run across processes and JSON
map backends. A framing or canonicalisation change requires a new version prefix.

## Gate and execution mode

`Gate::admit(&Principal, &Operation)` returns `Admission::Allow`, `Deny` or `Pending`.
`DefaultGate` admits the descriptor's principal kinds. The effect defaults therefore
deny agents administrative and destructive operations; an explicit empty principal
list admits nobody. A custom gate may override these defaults and is consulted on
every valid call. Pending approval refuses without dispatch and may be retried.

Supply a gate in `ComposeOptions` to `compose_with_options`. Its execution mode
defaults to `ExecutionMode::InProcess`, which refuses daemon-local operations.
A running daemon uses `ExecutionMode::Daemon` to serve both localities. Set clones
share the gate and fixed execution mode. A gate can retain synchronised policy
state when admission must change between calls.

## Call order and refusals

`set.call(&selection, principal, name, input, sink, cancel, preview).await` returns
`CallResult`, a result containing an `Outcome` or `Refusal`. The checks run in this
order: selection membership, input schema, gate, preview support, locality, handler,
successful output schema. The handler receives the authenticated principal, its own
operation name, input, sink, cancellation signal and preview flag.

| Code | Category | Condition |
|---|---|---|
| `unknown_operation` | `not_found` | Name outside the selection, or selection from another set |
| `invalid_input` | `invalid_input` | Input fails its schema |
| `not_admitted` | `refused` | Gate denies admission |
| `admission_pending` | `refused` | Gate leaves approval pending; retryable |
| `preview_unsupported` | `usage` | Preview requested for an operation without preview support |
| `no_daemon` | `unavailable` | Daemon-local operation in an in-process host |
| `invalid_result` | `internal` | Successful output fails its schema, including absent required output |
| `undeclared_refusal` | `internal` | Handler returns a code absent from its descriptor |
| `cancelled` | `interrupted` | Cancellation before or during dispatch; retryable |

Pre-dispatch refusals carry `not_dispatched` evidence. In-flight cancellation carries
`uncertain` evidence and drops the handler future; it cannot establish whether effects
committed. If cancellation and completion are both ready at a poll, cancellation wins.
Returned outcomes retain their evidence, including when output validation fails.

A declared handler refusal keeps its code, message, details, retryability and evidence;
its category is taken from the descriptor. An undeclared refusal is placed in the
`details` of `undeclared_refusal`, preserving its evidence on the wrapper. Handler
codes that resemble runner codes follow the same rule. Runner-generated refusals do
not need declarations. Tool-reported error outcomes retain their error shape and are
not validated as successful results.

The asynchronous sink acknowledges each record before the handler proceeds, providing
backpressure. Handlers must await sink delivery to preserve record order. A propagated
sink refusal must be declared by the operation; if delivery failure leaves effects
unresolved, it must carry `uncertain` evidence. Streaming cancellation tests hold and
release explicit futures, without timers or retries.
