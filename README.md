# Clapham

Clapham defines transport-independent application operations and exposes them through MCP,
a generated CLI and direct Rust calls. It is part of the
[London Project](https://github.com/thirtytwobits/london).

Operations have typed Rust input/output, generated schemas, scope requirements, resource
policies and declared refusals. JSON and typed Rust builders configure the same composition
model. Applications can load JSON and extend it programmatically before atomic validation.

OAuth establishes authority. The shared runner enforces scopes and application resource
policy before dispatch. rmcp implements MCP protocol machinery; the OAuth library implements
introspection requests. Applications own their authorisation server, credentials, account and
resource policy, persistence, workspace ownership and lifecycle.

The CLI discovers its authenticated service and derives commands, help, argument binding,
completions and presentation. Against a daemon it is Clapham's MCP client, and the records
extension preserves ordered records, refusal codes and effect evidence. Remote MCP contributions retain pinned definitions,
credential-scoped revisions and explicit controls over replay.

## Development

The core, CLI and MCP libraries require Rust 1.95. Repository gates use Rust 1.99.0.
Clapham is under development.

- [Typed Rust quickstart](crates/clapham/README.md#quickstart)
- [Runnable typed application](examples/typed/src/main.rs)
- [JSON plus Rust application](examples/lists/src/main.rs)
- [Configuration and OAuth setup](docs/composing.md)
- [Operation contract and test evidence](docs/contract.md)
- [MCP](docs/mcp.md)
- [Conformance](docs/conformance.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)

## Licence

MIT. See [LICENSE](LICENSE).
