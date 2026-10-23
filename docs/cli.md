# CLI command tree and input

`clapham-cli` builds a clap command tree from a composed set. The application holds
workspace exclusivity before composing and handing that set to `Cli::new`. Its
principal is normally `process_principal()`, the process account as a human caller.
`--as agent:bot` selects an agent, subject to the same gate as any other caller.

An operation name splits at every dot into command levels. `lists.get` is
`<app> lists get`; `lists.document.edit` is `<app> lists document edit`. The built-in
`clapham.describe` is also available as `<app> describe`. A command may itself be
an operation while also having subcommands; property arguments on the parent
cannot be combined with a child invocation.
The top-level `describe` name is reserved; a contribution under `describe.*`
refuses CLI construction as `cli_configuration`. Required positionals must precede
optional ones. Boolean properties take explicit `true` or `false` values;
presence-only switches are not generated.

Command syntax, malformed JSON and flag type-conversion errors return `usage`
(exit 2). Validation of the complete bound input retains the core's
`invalid_input` refusal (exit 65), including for enum constraints, nested JSON
types and missing or extra document properties. Handler and call-runner refusals
also retain their codes and evidence. Every parsing or input-validation failure
precedes admission and dispatch. Whole-object JSON is bounded by
`bounds.message_bytes` whether supplied inline or read from stdin.

The [crate guide](../crates/clapham-cli/README.md) describes argument types,
JSON input, help and the in-process API. Over the neutral contribution:

```console
lists-app lists get tasks
lists-app lists create --json '{"list":{"id":"tasks","name":"Tasks"}}'
lists-app lists create --list-json '{"id":"tasks","name":"Tasks"}'
lists-app lists get --json -
lists-app --as agent:bot lists compact
```

These illustrate invocations of a host's tree; the example binary is implemented
separately. The last call refuses as `not_admitted` before changing anything.
Daemon-local operations refuse as `no_daemon`, including if the host accidentally
supplies a daemon-mode set. Workspace locks remain the application's responsibility.

The CLI tests cover nested names, descriptor and property text in help, ordered
positionals, scalar and JSON types, enum/schema constraints, principal selection,
locality and the shared call runner. The conformance tests additionally drive the
real lists-and-items contribution through `Cli::run_from`. Tests use in-memory
arguments/readers and explicit cancellation, without clocks or retries.

Output rendering, confirmation and dry-run flags, timeouts, process tests and shell
completion commands are tracked in issue #10. The current API returns outcomes,
records, help or typed refusals to its host without printing or exiting.
