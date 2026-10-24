# Clapham

Clapham runs an application's operations, and builds the application's command line, MCP server
and MCP client from them. It is a piece of the
[London Project](https://github.com/thirtytwobits/london).

## Operations

An operation is a named action with a JSON Schema for its input and an optional result schema. Each
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
its records to the caller as they arrive, and validates successful structured output against its
schema. The gate
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
Flags come from the input schema and `--help` from the descriptor. Output defaults to human
fields or tables on a terminal and NDJSON envelopes in a pipe. A refusal ends the process
with its category's exit code. The effect
sets the safeguards:

| Effect | Command line | MCP annotations |
|---|---|---|
| `query` | runs | `readOnlyHint`, `idempotentHint` |
| `mutation` | runs; `--dry-run` where the operation accepts one | `destructiveHint: false` |
| `administrative` | asks for `--yes` | `destructiveHint: false`; the class in `_meta` |
| `destructive` | asks for `--yes` | `destructiveHint: true` |

The application owns the workspace lock. The 0.1.0 command line runs workspace operations in
its own process and refuses daemon-local operations as `no_daemon`. Daemon discovery and
CLI-to-daemon routing are not implemented in this release.

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
same calls through each front end and checks results, admitted-operation refusals, gate
visibility and ordered records where carried. The contract documents the native refusal
when a hidden remote tool cannot be bound.

## Status

Clapham 0.1.0 is the first stable release. It contains the core contract and call runner,
descriptor-derived workspace CLI, MCP server and client over stdio and Streamable HTTP,
and the reusable six-front-end conformance harness.
Schemas, the MCP wire record and public API snapshots are checked by repository gates.
The [lists application](examples/lists/README.md) is the worked example and process fixture.

Applications supply workspace ownership, authentication, policy, persistence and shutdown.
Daemon routing, durable approval coordination and transactional effects are outside 0.1.0.
[Release metadata](release.json), the [changelog](CHANGELOG.md) and
[release procedure](RELEASING.md) describe tagging and manually publishing the gated draft.

## Documentation

| Document | Purpose |
|---|---|
| [API reference](https://thirtytwobits.github.io/clapham/) | Versioned Rust API documentation for all five library crates |
| [0.1.0 contract](docs/contract.md) | Guarantees and their corresponding tests; updated with contract changes |
| [Composing an application](docs/composing.md) | Contributions, sets, selections and the application's `main` |
| [Command line](docs/cli.md) | Descriptor mapping, binding, output and safeguards; syntax comes from `--help` |
| [MCP](docs/mcp.md) | Server mapping/authentication and the extracted client contract |
| [Conformance](docs/conformance.md) | Run a contribution through the reusable harness |
| [Using a release](docs/using-a-release.md) | Dependencies by tag, conformance dev-dependencies and local patches |
| [Workspace guide](docs/README.md) | Crate boundaries, detailed references and validation |
| [Contributing](CONTRIBUTING.md) | Cargo gates, minimum-version checks and generated artefacts |
| [Security](SECURITY.md) | Private vulnerability reporting |

## Depending on it

Consumers take crates from the same published Git tag and commit their `Cargo.lock`.
The example below uses `v0.1.0`. Choose a published tag from
[releases](https://github.com/thirtytwobits/clapham/releases).

```toml
[dependencies]
clapham = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }
clapham-cli = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }

[dev-dependencies]
clapham-conformance = { git = "https://github.com/thirtytwobits/clapham", tag = "v0.1.0" }
```

`clapham` and `clapham-cli` require Rust 1.85. The MCP crates and conformance library require
Rust 1.88. Both MCP transport features, `stdio` and `http`, are enabled by default and can
be selected independently. Repository gates use Rust 1.99.0; consumers do not need the gate
toolchain. See [using a release](docs/using-a-release.md) for the complete setup.

## Licence

MIT. See [`LICENSE`](LICENSE).
