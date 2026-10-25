# Calls

External adapters authenticate before constructing `OAuthContext`. Its issuer, audience,
subject, client, scopes and extension claims come from trusted OAuth verification or
introspection. The application retains account policy and object-specific access checks.

The set runner checks selection, OAuth resource binding, required scopes, input schemas and
conversion, host admission, preview/locality and typed resource policies before dispatch.
Cancellation is checked before entry and before dispatch. Successful output is validated.
The low-level operation invocation API is for trusted dispatch, not an external authority boundary.

Resource policies receive typed input and verified authority. Discovery cannot evaluate
resource checks requiring input; invocation must still pass those checks. Asynchronous policy
cancellation is not-dispatched because the operation handler has not run.

Declared refusals retain their code; descriptor categories determine CLI exit codes.
Undeclared handler refusals become `undeclared_refusal`, with original details and evidence.
Typed conversion failures are dispatcher errors and retain `invalid_input` or `invalid_result`.

Evidence records dispatch knowledge: `not_dispatched`, `completed`, `uncertain`. A completed
response does not promise durable effects. In-flight cancellation and lost remote replies
leave effects uncertain. Reconcile effects before issuing another mutation.

Records are delivered through an awaited sink. The operation HTTP adapter streams bounded
NDJSON record frames followed by one outcome/refusal. Each record carries a one-use ticket.
The CLI acknowledges with the same verified OAuth authority after its output sink accepts the
record, and the producer waits for that acknowledgement. MCP uses its native final-result representation;
explicit effect evidence is carried in result metadata.

Canonical SHA-256 revisions sort nested object keys and definition order. The framing domains
are `clapham.composed-set.v1` and `clapham.selection.v1`, followed by a zero byte and canonical
compact JSON. These identify contract data, not permission grants or completed effects.

See [test evidence](contract.md) and [MCP](mcp.md).

## Typed invocation

A trusted host can call a typed operation using authority established by its OAuth middleware:

```rust
use clapham::{ComposedSet, OAuthContext, Refusal, TypedOperation};
use serde::{Deserialize, Serialize};
use schemars::JsonSchema;
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CountInput {}

async fn count_lists(
    set: &ComposedSet,
    authority: OAuthContext,
    count: &TypedOperation<CountInput, u64>,
) -> Result<u64, Refusal> {
    count.call(set, authority, CountInput {}).await
}
```

`call_outcome` also preserves adapter content, error status and effect evidence.
