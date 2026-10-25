# Command line

`Cli::connect` derives its commands from an authenticated operation service. `HttpBackend`
uses an OAuth token supplied by the host's credential store. The service owns execution,
resource access and shared validation. `Cli::new` supports trusted in-process embedding with
verified authority supplied by its host.

Commands split complete operation names at dots. Flags derive from the input schema;
positionals, descriptions and titles supply presentation. Scalar binding retains JSON types.
Object/array inputs accept JSON values. Whole-object JSON and individual properties conflict.
Use the generated `--help` for syntax.

Confirmation and unsupported-preview checks precede stdin reads. Confirmation does not grant
permission. Remote calls send only operation arguments and preview; the OAuth grant determines
authority. Discovery-hidden commands fail as usage errors before dispatch.

Terminal output uses fields or tables. Pipes use versioned NDJSON envelopes. Ordered records
are flushed before the sink acknowledges them, followed by one terminal result. Domain
refusals are unchanged JSON on stderr and use their category's exit code. Human renderers
can be registered per operation without changing structured output.

Cancellation and timeouts affect only the invocation. Lost delivery leaves effects uncertain;
HTTP redirects and retries are disabled. See [the library](../crates/clapham-cli/README.md),
[application guide](composing.md) and [contract](contract.md).
