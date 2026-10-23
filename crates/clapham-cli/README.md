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
an explicit `true` or `false`. Positional properties appear in descriptor order.
Object and array properties use `--<name>-json <document>`. A local JSON Pointer
`$ref` supplies its target's type; complex, unconstrained or union properties use
the same JSON argument form.

`--json <document>` supplies the whole input object instead of property arguments;
`--json -` reads it from the supplied stdin reader, bounded by `bounds.message_bytes`.
Required properties remain required in either form. Mixing the two forms refuses
as `usage`. Optional properties are omitted rather than populated with schema
defaults. The composed operation's validator checks the complete input before any
admission or dispatch. Invalid arguments, JSON, types, enum values and schema
constraints refuse as `usage`, with not-dispatched evidence and exit code 2.

Malformed command segments, unknown or repeated positional properties, invalid
flag spellings and flag collisions refuse at construction as `cli_configuration`
(exit 78). `--help`, `--json` and `--as` are reserved; JSON property flags must also
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
generation. This library does not print or exit the process. Output rendering,
effect confirmations, dry runs, timeouts and packaged shell completions follow in
the CLI output-and-safeguards implementation.
