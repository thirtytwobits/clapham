# Clapham

Clapham runs an application's operations, and builds the application's command line, MCP server
and MCP client from them. It is a piece of the
[London Project](https://github.com/thirtytwobits/london).

## Operations

An operation is a named action with a JSON Schema for its input and one for its result. Each
London piece supplies its operations under its own namespace, such as
`clerkenwell.document.edit`, `walthamstow.workspace.upgrade` or `battersea.run.cancel`, and the
application adds its own.

A descriptor states each operation as data:

- its name, title and description; a model reads the description to decide whether to call it
- the input and result schemas
- its effect: `query`, `mutation`, `administrative` or `destructive`
- the kinds of principal it admits: `human`, `agent`, `service`, `system`
- where it runs: against the workspace, or in the running daemon
- the refusal codes it returns, each with its category
- whether it streams records before its result, accepts a dry run, or is read as an MCP resource
- the input properties the command line takes as positional arguments

`operation.schema.json` publishes the descriptor, so a host in any language reads and writes it.

## What Clapham does

**Composes.** At start-up the application hands Clapham every piece's operations. Clapham checks
each descriptor, compiles its schemas and builds one registry, which stays fixed while the
application runs. A namespace claimed twice, a malformed name or a schema that fails to compile
stops composition with its own error code.

**Runs every call.** Clapham validates the input against its schema, asks the application's gate
whether the caller's principal may run the operation, runs the operation as that principal, passes
its records to the caller as they arrive, and validates the result against its schema. The gate
answers allow, deny or pending; by default it admits the principal kinds the descriptor names.

**Refuses precisely.** A refusal carries the code of the piece that refused, unchanged, such as
`stale_write`, `workspace_held` or `tool_approval_pending`, and the category the descriptor
declares for it. Each front end acts on the category:

| Category | Exit code | Meaning |
|---|---|---|
| `usage` | 2 | the invocation was wrong |
| `invalid_input` | 65 | the input failed its schema or the operation's checks |
| `not_found` | 66 | what the input names is absent |
| `conflict` | 67 | a fence failed; the details carry what a retry needs |
| `refused` | 77 | the gate, a policy or an approval said no |
| `unavailable` | 69 | the daemon is down, the workspace is held, or the remote is down |
| `interrupted` | 75 | cancelled, timed out or stopped by shutdown |
| `configuration` | 78 | the application's configuration is wrong |
| `internal` | 70 | a fault, including a refusal code the descriptor omits |

**Builds the command line.** One subcommand per operation, built at run time from the registry.
Flags come from the input schema and `--help` from the descriptor. Output is JSON on a terminal
and NDJSON in a pipe, and a refusal ends the process with its category's exit code. The effect
sets the safeguards:

| Effect | Command line | MCP annotations |
|---|---|---|
| `query` | runs | `readOnlyHint`, `idempotentHint` |
| `mutation` | runs; `--dry-run` where the operation accepts one | `destructiveHint: false` |
| `administrative` | asks for `--yes` | `destructiveHint: false`; the class in `_meta` |
| `destructive` | asks for `--yes` | `destructiveHint: true` |

The command line runs workspace operations in its own process under the workspace lock, and
reaches the running daemon over MCP for the rest.

**Serves MCP.** Each operation is a tool, named as the operation is, with annotations from its
effect; a query with a resource template is also a resource. The server speaks stdio and
Streamable HTTP, and lists for each caller the operations the gate admits to that caller.

**Connects to MCP servers.** Clapham's MCP client binds a remote server's tools into the registry
as operations, under the namespace the application gives that connection. The command line, the
MCP server and Battersea's flows then call them as they call local operations.

**Binds callers to principals.** The operator at a shell is `human:<account>`. An HTTP caller is
the principal the application's authenticator maps its credential to. A stdio connection is the
principal its launch configuration names. Every operation runs as its caller's principal, and
Clerkenwell decides what that principal may change.

**Proves its front ends.** A conformance suite, over a contribution of lists and items, runs the
same calls through each front end and checks each one returns the same result, carries refusals
unchanged and enforces the gate.

## Status

The Cargo workspace, repository gates, core contract, composition and call runner are in place,
with published operation, refusal and catalogue schemas. Every set includes `clapham.describe`
for the caller's admitted catalogue and `clapham.version` for library and application identity.
Calls enforce selection, admission, preview support, locality and the declared refusal contract.
The neutral lists-and-items contribution exercises the core call contract with scripted
concurrency. The CLI builds its command tree, help and input binding from those descriptors,
and runs calls in-process with confirmations, previews, deadlines, output formatting
and generated shell completions. The MCP adapters, the remaining
conformance harness and the lists example follow in their implementation issues.
[thirtytwobits/london#2](https://github.com/thirtytwobits/london/issues/2) holds the design
and the plan for the first release. See [CONTRIBUTING.md](CONTRIBUTING.md) for the gate commands
and [the workspace guide](docs/README.md) for the crate boundaries.

## Licence

MIT. See [`LICENSE`](LICENSE).
