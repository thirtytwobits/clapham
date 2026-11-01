# clapham-cli

A schema-derived command line for an authenticated operation service. Requires Rust 1.95.

`Cli::connect` discovers the operations visible to the connection's OAuth grant and builds
commands, argument binding, help and completions. `McpBackend` calls the daemon's MCP server,
over Streamable HTTP with a token supplied by the application's OAuth client or credential
store, or over the standard streams of a daemon the host launched.

```no_run
use clapham::Bounds;
use clapham_cli::{Cli, McpBackend};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let backend = McpBackend::http(
    std::env::var("CLAPHAM_ENDPOINT")?,
    std::env::var("CLAPHAM_ACCESS_TOKEN")?,
)
.await?;
let cli = Cli::connect("lists", backend, Bounds::default()).await?;
let exit_code = cli.run().await;
# let _ = exit_code;
# Ok(())
# }
```

The `Backend` trait supports other authenticated operation interfaces. `Cli::new` is the
trusted in-process embedding path: the host supplies a composed set and authority already
established by OAuth middleware. Production executable clients use `Cli::connect`.

Scalar schema properties become typed flags; positional metadata supplies positionals.
Objects, arrays and unions accept JSON-valued arguments. Whole-object JSON and individual
properties are mutually exclusive. Local references are resolved for binding; the complete
input is schema-validated before dispatch. The command's `--help` describes syntax.

Effects and explicit confirmation settings supply CLI safeguards. Preview requires declared
support. Timeouts cancel the invocation; a lost remote response leaves effects uncertain.
HTTP redirects and retries are disabled. Endpoint URLs require HTTPS, except for loopback
HTTP development. Token values are omitted from debug output.

The daemon delivers ordered records under the records extension before one terminal outcome
or refusal. Record delivery is awaited and flushed before acknowledgement. Cancelling closes
the call's response stream, or sends `notifications/cancelled` over stdio. Human output uses fields/tables on terminals; piped output uses versioned NDJSON
`{v:1,kind:operation_name,data:value}` frames. Domain refusal codes and categories are retained,
and categories determine exit codes. Custom renderers affect human output only.

Discovery-hidden commands fail locally as usage errors. Calls to visible operations retain
server refusals. See the [contract](../../docs/contract.md) and [application guide](../../docs/composing.md).
