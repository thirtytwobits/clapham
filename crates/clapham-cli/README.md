# clapham-cli

Build a command tree at run time from a composed set. `lists.get` becomes
`<app> lists get`; further dots add command levels. The top-level `describe`
command also invokes `clapham.describe`.

The host takes workspace exclusivity and composes contributions that hold the
workspace, then constructs `Cli::new(name, set, principal)`. Use `process_principal()`
for the operating-system account as a human principal. `--as <kind>:<id>` anywhere
in the command selects a human, agent, service or system principal. The composed
set's gate still decides admission.

## Input and help

Each operation's title, description, property descriptions and effect supply its
help. Scalar properties become `--<name> <value>`: strings stay strings; integers,
numbers, booleans and enums retain their JSON types and constraints. Booleans take
an explicit `true` or `false`. Positional properties appear in descriptor order,
with required properties before optional ones.
Object and array properties use `--<name>-json <document>`. A local JSON Pointer
`$ref` supplies its target's type unless the property supplies its own `type`;
complex, unconstrained or union properties use
the same JSON argument form.

`--json <document>` supplies the whole input object instead of property arguments;
`--json -` reads it from the supplied stdin reader. Both whole-object JSON forms
are bounded by `bounds.message_bytes` before parsing.
Required properties remain required in either form. Mixing the two forms refuses
as `usage`. Optional properties are omitted rather than populated with schema
defaults. The composed operation's validator checks the complete input before any
admission or dispatch. Command syntax, malformed JSON and flag type-conversion
errors refuse as `usage` (exit 2). Once bound, the complete input is validated;
schema failures, including enum constraints, nested JSON types and missing or
extra document properties, retain the core's `invalid_input` refusal (exit 65).
Both carry not-dispatched evidence. The call runner validates again because an
`Invocation` can also be constructed or edited by the host.

The top-level `describe` and `completions` names are reserved; contributions under
those namespaces refuse CLI construction. Malformed command segments, unknown or
repeated positional properties, a required positional after an optional one,
invalid flag spellings and flag collisions refuse as `cli_configuration` (exit 78).
`--help`, `--json`, `--as`, `--yes`, `--dry-run`, `--timeout`, `--format` and `--raw`
are reserved; JSON property flags must also
be distinct from scalar flags, so `data` as an object and `data-json` as a scalar
cannot coexist as flags.

## Calling

`parse_from(args, stdin)` takes argv including the program name and returns an
`Invocation`. `call(invocation, records, cancel).await` dispatches it through the
ordinary selection, gate and call runner. `run_from` combines the two. Workspace
operations execute in-process; daemon-local operations refuse as `no_daemon`.
Call refusals retain their codes, categories and evidence unchanged. The caller
supplies the executor, record sink and cancellation signal.

```rust
use clapham::{Bounds, Cancellation, Principal, PrincipalKind, RecordSink, compose};
use clapham_cli::Cli;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
let set = compose([], Bounds::default())?;
let cli = Cli::new("lists-app", set, Principal::new("operator", PrincipalKind::Human))?;
let invocation = cli.parse_from(["lists-app", "describe"], std::io::empty())?;
assert_eq!(invocation.name, "clapham.describe");
let outcome = futures_executor::block_on(cli.call(
    invocation, RecordSink::default(), Cancellation::new(),
))?;
assert!(outcome.result.unwrap().is_array());
# Ok(())
# }
```

Help returns `CliError::Help(text)` with exit code 0 and never reads stdin or
dispatches. Other errors are `CliError::Refusal`; `exit_code()` uses the refusal's
category. `command()` returns a detached clap tree for inspection and completion
generation. These value-oriented methods leave printing to the host.

## Safeguards and deadlines

Administrative and destructive operations require `--yes`, including previews.
Without it, parsing refuses as `usage` before reading stdin or running anything.
`--dry-run` sets the call's preview flag only where the descriptor supports it;
otherwise it also refuses as `usage`. Confirmation does not change admission:
an agent's `lists compact --yes` can still be refused as `not_admitted`.
`call` checks the same safeguards for invocations constructed by the host.

`--timeout <duration>` accepts durations such as `500ms`, `1.25s` or `2m` and
cancels the supplied call signal. Its budget starts when `call` begins, after
parsing and safeguards; it does not bound stdin reads or blocking output writes.
Zero cancels before dispatch. In-flight expiry uses the core's `cancelled`
refusal, including retryability and uncertain evidence. Cancellation has priority
when both deadline and completion are ready. The default deadline uses a helper
thread and needs no async runtime. `with_deadline` lets a host integrate its own
deadline future; tests use explicitly signalled futures instead of timers.

## Process output

`run().await` takes process arguments and standard streams, detects whether
stdout is a terminal and returns an exit code. The host returns that code from
`main`; the library never terminates the process. `execute_from` provides the same
runner with injected arguments, streams, terminal status and cancellation signal.

```no_run
use clapham::{Bounds, compose};
use clapham_cli::{Cli, process_principal};

fn main() -> Result<std::process::ExitCode, Box<dyn std::error::Error>> {
    // Compose the host's held-workspace contributions here.
    let cli = Cli::new("lists-app", compose([], Bounds::default())?, process_principal()?)?;
    Ok(std::process::ExitCode::from(futures_executor::block_on(cli.run())))
}
```

JSON frames are `{ "v": 1, "kind": "lists.get", "data": ... }`. Records and
the final result use the operation's complete name as `kind`; records are ordered
and the final frame follows them. Ordinary results keep their declared JSON shape
in `data`. An absent structured result, content blocks, a reported error or
non-default evidence instead places the complete `Outcome` in `data`, preserving
those semantics. A completed outcome returns exit 0, including a tool-reported
error outcome; typed refusals use their category's exit code.

| Selection | Output |
|---|---|
| Default, piped | One compact JSON frame per line |
| Default, terminal | Human fields or tables |
| `--format json` | Pretty JSON on a terminal, compact otherwise |
| `--format ndjson` | One compact JSON frame per line |
| `--format human` | Human output regardless of terminal status |
| `--raw` | Omit the envelope in JSON modes; human output is unchanged |

Streaming operations always keep JSON records and the final result on individual
lines, even with `--format json` on a terminal. Human output uses output-schema
properties and titles, making tables for arrays of objects and `key: value` fields
for objects. Control characters are escaped. `with_renderer(name, callback)`
replaces human rendering for an operation; the callback receives its descriptor,
the data and whether it is a record. JSON modes retain the original values.

Each frame is written and flushed before acknowledging its record. A failed
record write cancels the producer and reports `output_failed` with uncertain
evidence; a failed final write preserves the completed outcome's evidence. These
are CLI delivery failures, not undeclared handler refusals. Refusals are serialised
unchanged as one compact JSON line on stderr, independent of output mode. The
public `write_refusal` helper also supports startup failures. A failed stderr
write still returns the original category's code.

The generated `schemas/envelope.schema.json` publishes the frame contract.
`cargo xtask schemas` writes it and `cargo xtask schemas --check` refuses stale
output alongside the core schemas.

## Completions

`<app> completions bash|zsh|fish|powershell` prints the corresponding script from
the live descriptor-built tree, including operation arguments and global controls,
without dispatching. The same scripts are available through `completions(Shell)`.
