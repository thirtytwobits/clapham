use super::*;
use clapham_mcp_client::{McpConfig, McpConnection, McpHttpOptions, McpSelection};
use serde_json::json;
use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// A host executable plus prefix arguments and environment, without shell interpretation.
#[derive(Debug, Clone)]
pub struct Process {
    /// Executable path; use Cargo's `CARGO_BIN_EXE_<name>` in integration tests.
    pub program: PathBuf,
    /// Arguments preceding the CLI invocation or `serve` command.
    pub args: Vec<OsString>,
    /// Fixture configuration shared with the host executable.
    pub env: BTreeMap<OsString, OsString>,
}
impl Process {
    /// A host executable using the documented reference process protocol.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).envs(&self.env).kill_on_drop(true);
        command
    }
}

/// Process transports supplied by the reference application contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessKind {
    /// Invoke the command line as a fresh process for each case.
    CommandLine,
    /// Drive MCP JSON-RPC over a server process's stdin and stdout.
    McpStdio,
    /// Drive authenticated Streamable HTTP over a server process's ephemeral loopback listener.
    McpHttp,
    /// Bind the stdio server through the MCP client and call its contribution.
    BoundStdio,
    /// Bind the HTTP server through the MCP client and call its contribution.
    BoundHttp,
}
impl ProcessKind {
    fn name(self) -> &'static str {
        match self {
            Self::CommandLine => "command_line",
            Self::McpStdio => "mcp_stdio",
            Self::McpHttp => "mcp_http",
            Self::BoundStdio => "bound_stdio",
            Self::BoundHttp => "bound_http",
        }
    }
}

/// Built-in process drivers. The executable recreates the same fixture and gate as the caller.
#[derive(Debug, Clone)]
pub struct ProcessFrontEnd {
    kind: ProcessKind,
    process: Process,
}
impl ProcessFrontEnd {
    /// Use `serve --stdio --as KIND:ID` or `serve --http 127.0.0.1:0` for server modes.
    /// HTTP credentials are `Bearer KIND:ID`; the host authenticator maps them explicitly.
    pub fn new(kind: ProcessKind, process: Process) -> Self {
        Self { kind, process }
    }
}
impl FrontEnd for ProcessFrontEnd {
    fn name(&self) -> &str {
        self.kind.name()
    }
    fn carries_records(&self) -> bool {
        self.kind == ProcessKind::CommandLine
    }
    fn binds_tools(&self) -> bool {
        matches!(self.kind, ProcessKind::BoundStdio | ProcessKind::BoundHttp)
    }
    fn observe<'a>(
        &'a self,
        fixture: &'a Fixture,
        _gate: Arc<dyn Gate>,
        case: &'a Case,
    ) -> ObservationFuture<'a> {
        Box::pin(async move {
            if self.kind == ProcessKind::CommandLine {
                return cli(&self.process, case).await;
            }
            let mut host = Host::start(&self.process, self.kind, &case.principal).await?;
            let observation = match self.kind {
                ProcessKind::McpStdio | ProcessKind::McpHttp => {
                    let catalogue = host
                        .request(
                            "tools/call",
                            json!({"name":"clapham.describe","arguments":{}}),
                            &case.principal,
                        )
                        .await?;
                    let catalogue = names(&catalogue["structuredContent"])?;
                    let tools = host.tools(&case.principal, &fixture.bounds).await?;
                    check_tools(&catalogue, &tools)?;
                    let result = host
                        .request(
                            "tools/call",
                            json!({"name":case.operation,"arguments":case.input}),
                            &case.principal,
                        )
                        .await?;
                    Observation {
                        catalogue,
                        frames: vec![Frame::Outcome(mcp_outcome(result)?)],
                        exit_code: None,
                    }
                }
                ProcessKind::BoundStdio | ProcessKind::BoundHttp => {
                    let mut config = McpConfig::new("remote", "conformance");
                    config.bounds = fixture.bounds.clone();
                    let connection = if let Some(endpoint) = &host.endpoint {
                        let mut options = McpHttpOptions::new(endpoint);
                        options.bearer_token = Some(principal(&case.principal));
                        McpConnection::http(options, config).await
                    } else {
                        let reader = host
                            .output
                            .take()
                            .ok_or_else(|| Failure::new("missing stdio reader"))?;
                        let writer = host
                            .input
                            .take()
                            .ok_or_else(|| Failure::new("missing stdio writer"))?;
                        McpConnection::stdio(reader, writer, Default::default(), config).await
                    }
                    .map_err(|error| Failure::new(error.to_string()))?;
                    let observation = bound(&connection, case, &fixture.bounds).await;
                    connection.close();
                    // Drop every binding/connection before awaiting EOF-based host shutdown.
                    drop(connection);
                    observation?
                }
                ProcessKind::CommandLine => unreachable!(),
            };
            host.finish().await?;
            Ok(observation)
        })
    }
}

fn principal(principal: &Principal) -> String {
    let kind = match principal.kind {
        PrincipalKind::Human => "human",
        PrincipalKind::Agent => "agent",
        PrincipalKind::Service => "service",
        PrincipalKind::System => "system",
    };
    format!("{kind}:{}", principal.id)
}
async fn cli_once(
    process: &Process,
    principal_value: &Principal,
    operation: &str,
    input: &Value,
) -> Result<(Vec<Frame>, i32), Failure> {
    let mut child = process
        .command()
        .arg("--as")
        .arg(principal(principal_value))
        .args(operation.split('.'))
        .args(["--yes", "--format", "ndjson", "--json"])
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // JSON travels on stdin, including inputs larger than Windows' argv limit.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Failure::new("missing CLI stdin"))?;
    let write = async move {
        let written = stdin.write_all(input.to_string().as_bytes()).await;
        drop(stdin);
        written
    };
    let (written, output) = futures_util::future::join(write, child.wait_with_output()).await;
    let output = output?;
    if output.status.success() {
        written?;
    }
    let code = output
        .status
        .code()
        .ok_or_else(|| Failure::new("CLI terminated without an exit code"))?;
    let mut values = Vec::new();
    for line in output
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let envelope: Value = serde_json::from_slice(line)?;
        if envelope["v"] != 1 || envelope["kind"] != operation || envelope.get("data").is_none() {
            return Err(Failure::new("invalid CLI envelope"));
        }
        values.push(envelope["data"].clone());
    }
    let mut frames: Vec<_> = values.into_iter().map(Frame::Record).collect();
    if code == 0 {
        if !output.stderr.is_empty() {
            return Err(Failure::new("successful CLI wrote stderr"));
        }
        let Some(Frame::Record(value)) = frames.pop() else {
            return Err(Failure::new("CLI returned no result"));
        };
        frames.push(Frame::CliResult(value));
    } else {
        let refusal: Refusal = serde_json::from_slice(&output.stderr)?;
        frames.push(Frame::Refusal(refusal));
    }
    Ok((frames, code))
}
async fn cli(process: &Process, case: &Case) -> Result<Observation, Failure> {
    let (described, exit) =
        cli_once(process, &case.principal, "clapham.describe", &json!({})).await?;
    if exit != 0 {
        return Err(Failure::new("CLI describe was refused"));
    }
    let [Frame::CliResult(value)] = described.as_slice() else {
        return Err(Failure::new("CLI describe returned unexpected frames"));
    };
    let catalogue = names(value)?;
    let (frames, exit_code) =
        cli_once(process, &case.principal, &case.operation, &case.input).await?;
    Ok(Observation {
        catalogue,
        frames,
        exit_code: Some(exit_code),
    })
}

struct Host {
    process: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    endpoint: Option<String>,
    client: Option<reqwest::Client>,
    id: u64,
}
impl Host {
    async fn start(
        process: &Process,
        kind: ProcessKind,
        caller: &Principal,
    ) -> Result<Self, Failure> {
        let http = matches!(kind, ProcessKind::McpHttp | ProcessKind::BoundHttp);
        let mut command = process.command();
        command.arg("serve");
        if http {
            command.args(["--http", "127.0.0.1:0"]);
        } else {
            command.args(["--stdio", "--as"]).arg(principal(caller));
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take();
        let output = Some(BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Failure::new("missing host stdout"))?,
        ));
        let mut host = Self {
            process: child,
            input,
            output,
            endpoint: None,
            client: if http {
                Some(
                    reqwest::Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .retry(reqwest::retry::never())
                        .build()
                        .map_err(|e| Failure::new(e.to_string()))?,
                )
            } else {
                None
            },
            id: 0,
        };
        if http {
            let mut line = String::new();
            host.output.as_mut().unwrap().read_line(&mut line).await?;
            let ready: Value = serde_json::from_str(&line)?;
            let address: std::net::SocketAddr = ready["address"]
                .as_str()
                .ok_or_else(|| Failure::new("missing HTTP readiness address"))?
                .parse()
                .map_err(|_| Failure::new("invalid HTTP readiness address"))?;
            if !address.ip().is_loopback() || address.port() == 0 {
                return Err(Failure::new(
                    "HTTP fixture must bind an ephemeral loopback port",
                ));
            }
            host.endpoint = Some(format!("http://{address}/mcp"));
        }
        Ok(host)
    }
    async fn tools(&mut self, caller: &Principal, bounds: &Bounds) -> Result<Vec<String>, Failure> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        for _ in 0..bounds.pages {
            let page = self
                .request("tools/list", json!({"cursor":cursor}), caller)
                .await?;
            tools.extend(names(&page["tools"])?);
            if tools.len() > bounds.tools {
                return Err(Failure::new("tools/list exceeds fixture bounds"));
            }
            match page.get("nextCursor") {
                None | Some(Value::Null) => return Ok(tools),
                Some(Value::String(next)) if seen.insert(next.clone()) => {
                    cursor = Some(next.clone());
                }
                _ => return Err(Failure::new("invalid or repeated tools/list cursor")),
            }
        }
        Err(Failure::new("tools/list exceeds fixture page bounds"))
    }
    async fn request(
        &mut self,
        method: &str,
        mut params: Value,
        caller: &Principal,
    ) -> Result<Value, Failure> {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{}});
        self.id += 1;
        let request = json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params});
        let response: Value = if let Some(endpoint) = &self.endpoint {
            let mut send = self
                .client
                .as_ref()
                .unwrap()
                .post(endpoint)
                .bearer_auth(principal(caller))
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", method)
                .header("accept", "application/json, text/event-stream");
            if let Some(name) = params.get("name").and_then(Value::as_str) {
                send = send.header("mcp-name", name);
            }
            send.json(&request)
                .send()
                .await
                .map_err(|e| Failure::new(e.to_string()))?
                .error_for_status()
                .map_err(|e| Failure::new(e.to_string()))?
                .json()
                .await
                .map_err(|e| Failure::new(e.to_string()))?
        } else {
            let input = self
                .input
                .as_mut()
                .ok_or_else(|| Failure::new("missing host stdin"))?;
            input.write_all(format!("{request}\n").as_bytes()).await?;
            input.flush().await?;
            loop {
                let mut line = String::new();
                if self.output.as_mut().unwrap().read_line(&mut line).await? == 0 {
                    return Err(Failure::new("MCP host closed before its response"));
                }
                let message: Value = serde_json::from_str(&line)?;
                if !notification(&message) {
                    break message;
                }
            }
        };
        if response["jsonrpc"] != "2.0"
            || response["id"] != self.id
            || response.get("method").is_some()
            || response.get("error").is_some()
        {
            return Err(Failure::new(format!("unexpected MCP response: {response}")));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| Failure::new("MCP response has no result"))
    }
    async fn finish(mut self) -> Result<(), Failure> {
        if let Some(mut input) = self.input.take() {
            input.shutdown().await?;
        }
        drop(self.input.take());
        let mut extra = Vec::new();
        if let Some(mut output) = self.output.take() {
            output.read_to_end(&mut extra).await?;
        }
        let status = self.process.wait().await?;
        let mut extra_result = false;
        for line in extra.split(|byte| *byte == b'\n') {
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            extra_result |= !notification(&serde_json::from_slice(line)?);
        }
        if !status.success() || extra_result {
            return Err(Failure::new(
                "host returned an extra result or unsuccessful exit",
            ));
        }
        Ok(())
    }
}
fn notification(message: &Value) -> bool {
    message["jsonrpc"] == "2.0"
        && message.get("method").is_some_and(Value::is_string)
        && message.get("id").is_none()
        && message.get("result").is_none()
        && message.get("error").is_none()
        && message.get("params").is_none_or(Value::is_object)
}
fn check_tools(catalogue: &[String], tools: &[String]) -> Result<(), Failure> {
    let unique = tools.iter().collect::<BTreeSet<_>>();
    if unique.len() != tools.len() || unique != catalogue.iter().collect::<BTreeSet<_>>() {
        return Err(Failure::new(
            "tools/list catalogue differs from clapham.describe",
        ));
    }
    Ok(())
}
fn mcp_outcome(value: Value) -> Result<Outcome, Failure> {
    if !value["content"].is_array() || value.get("isError").is_some_and(|flag| !flag.is_boolean()) {
        return Err(Failure::new("invalid MCP tool result"));
    }
    // The server contract always carries the structured result when the local outcome has one.
    let mut outcome = Outcome::new(Value::Null);
    outcome.result = value.get("structuredContent").cloned();
    outcome.is_error = value["isError"].as_bool().unwrap_or(false);
    Ok(outcome)
}
async fn bound(
    connection: &McpConnection,
    case: &Case,
    bounds: &Bounds,
) -> Result<Observation, Failure> {
    let catalogue = connection
        .discover()
        .await
        .map_err(|e| Failure::new(e.to_string()))?;
    let mut describe_selection = McpSelection::new("clapham.describe");
    describe_selection.alias = Some("describe".into());
    describe_selection.principals = Some(vec![case.principal.kind]);
    let describe = catalogue
        .bind(vec![describe_selection])
        .map_err(|e| Failure::new(e.to_string()))?;
    let set = compose_with_options(
        [describe.contribution()],
        bounds.clone(),
        ComposeOptions {
            execution: ExecutionMode::Daemon,
            ..Default::default()
        },
    )
    .map_err(|e| Failure::new(e.to_string()))?;
    let described = set
        .call(
            &set.all(),
            case.principal.clone(),
            "remote.describe",
            json!({}),
            RecordSink::default(),
            Cancellation::new(),
            false,
        )
        .await
        .map_err(|e| Failure::new(e.to_string()))?;
    if described.is_error {
        return Err(Failure::new("remote describe was refused"));
    }
    let names = names(
        described
            .result
            .as_ref()
            .ok_or_else(|| Failure::new("remote describe returned no result"))?,
    )?;
    let mut selection = McpSelection::new(&case.operation);
    selection.alias = Some("call".into());
    selection.principals = Some(vec![case.principal.kind]);
    let result = match catalogue.bind(vec![selection]) {
        Err(error) => Err(error.refusal(clapham::Evidence::NotDispatched)),
        Ok(binding) => {
            let set = compose_with_options(
                [binding.contribution()],
                bounds.clone(),
                ComposeOptions {
                    execution: ExecutionMode::Daemon,
                    ..Default::default()
                },
            )
            .map_err(|e| Failure::new(e.to_string()))?;
            set.call(
                &set.all(),
                case.principal.clone(),
                "remote.call",
                case.input.clone(),
                RecordSink::default(),
                Cancellation::new(),
                false,
            )
            .await
        }
    };
    Ok(Observation {
        catalogue: names,
        frames: vec![terminal(result)],
        exit_code: None,
    })
}
