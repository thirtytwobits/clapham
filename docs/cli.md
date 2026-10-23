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
The top-level `describe` and `completions` names are reserved; a contribution under either
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
lists-app --as agent:bot lists compact --yes
lists-app lists create --list-json '{"id":"preview","name":"Preview"}' --dry-run
lists-app lists delete --id tasks --yes
lists-app lists export --id tasks --format ndjson
lists-app completions zsh
```

These illustrate invocations of a host's tree; the example binary is implemented
separately. The agent's compact call refuses as `not_admitted` before changing anything.
Daemon-local operations refuse as `no_daemon`, including if the host accidentally
supplies a daemon-mode set. Workspace locks remain the application's responsibility.

The CLI tests cover nested names, descriptor and property text in help, ordered
positionals, scalar and JSON types, enum/schema constraints, principal selection,
locality and the shared call runner. The conformance tests additionally drive the
real lists-and-items contribution through `Cli::run_from`. Tests use in-memory
arguments/readers and explicit cancellation, without clocks or retries.

`run` and `execute_from` add process output and return the exit code to the host.
Pipes use version-one `{v, kind, data}` JSON frames; terminals default to human
tables or fields. Explicit JSON is pretty on a terminal, while streaming JSON
always stays one frame per line. `--raw` omits the JSON envelope and leaves human
output unchanged. `with_renderer`
registers an operation's human presentation. Refusals always use one unchanged
JSON line on stderr and the category's code. The published frame schema is
`crates/clapham-cli/schemas/envelope.schema.json`, generated and checked by xtask.

Administrative and destructive effects require `--yes`. Parsing checks confirmation
and preview support before reading stdin, validating input or checking admission.
An unconfirmed agent's `lists compact` therefore gets `usage`; with `--yes`, it gets
`not_admitted`. Unsupported `--dry-run` returns `preview_unsupported` in category
`usage` (exit 2). Prepared calls keep the early confirmation check, while the core
checks preview support after input validation and admission. Confirmation does not
change the gate's policy.

`--timeout` applies to the async call after parsing and confirmation. It does not
bound stdin or pre-empt blocking work. Record writes run inside the timeout budget,
so their time counts towards it even though expiry cannot interrupt a blocked
write. Final result writes happen after the call completes. Expiry and external
interrupts both return `cancelled` with the core's evidence rules; there is no
separate timeout refusal code. A call-local signal isolates timeouts and delivery
failures from the host's signal, while host cancellation still reaches the call.
The host may replace the deadline future with `with_deadline` to integrate its
runtime or explicit test signals.

The process fixture seeds lists and items, runs the real CLI and records its final
state for assertions. Process tests check every category's exit code, one-line
stderr, ordered export frames, raw/human/JSON output, confirmation, preview,
completions and an entered write cancelled by an explicitly signalled deadline.
Terminal-mode tests inject stdout's terminal status; `run` uses `IsTerminal` in
normal processes. Output-failure tests prove cancellation stops the producer after
a failed record write. No test sleeps, starts a timer or reads the wall clock.
