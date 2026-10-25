use super::*;
use clapham_mcp_client::{McpConfig, McpConnection, McpHttpOptions, McpSelection};
use clapham_mcp_server::{ACKNOWLEDGE_REQUEST, RECORD_NOTIFICATION, RECORDS_EXTENSION};
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
        command
            .args(&self.args)
            .envs(&self.env)
            .env("CLAPHAM_FIXTURE_MODE", "1")
            .kill_on_drop(true);
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
    /// Fixture hosts bind scripted verified claims through their private environment.
    /// HTTP credentials use the same fixture-only claim snapshot.
    pub fn new(kind: ProcessKind, process: Process) -> Self {
        Self { kind, process }
    }
}
impl FrontEnd for ProcessFrontEnd {
    fn name(&self) -> &str {
        self.kind.name()
    }
    fn carries_records(&self) -> bool {
        true
    }
    fn binds_tools(&self) -> bool {
        matches!(self.kind, ProcessKind::BoundStdio | ProcessKind::BoundHttp)
    }
    fn hidden_refusal_code(&self) -> Option<&str> {
        match self.kind {
            ProcessKind::CommandLine => Some("usage"),
            ProcessKind::BoundStdio | ProcessKind::BoundHttp => Some("mcp_selection"),
            _ => None,
        }
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
            let mut host = Host::start(&self.process, self.kind, &case.auth).await?;
            let observation = match self.kind {
                ProcessKind::McpStdio | ProcessKind::McpHttp => {
                    let catalogue = host
                        .request(
                            "tools/call",
                            json!({"name":"clapham.describe","arguments":{}}),
                            &case.auth,
                        )
                        .await?;
                    let catalogue = names(&catalogue.result()?["structuredContent"])?;
                    let tools = host.tools(&case.auth, &fixture.bounds).await?;
                    check_tools(&catalogue, &tools)?;
                    let (records, result) =
                        host.call(&case.operation, &case.input, &case.auth).await?;
                    let mut frames: Vec<_> = records.into_iter().map(Frame::Record).collect();
                    frames.push(match result {
                        HostReply::Result(value) => Frame::Outcome(mcp_outcome(value)?),
                        HostReply::Refusal(refusal) => Frame::Refusal(refusal),
                    });
                    Observation {
                        catalogue,
                        frames,
                        exit_code: None,
                    }
                }
                ProcessKind::BoundStdio | ProcessKind::BoundHttp => {
                    let mut config = McpConfig::new("remote", "conformance");
                    config.bounds = fixture.bounds.clone();
                    let connection = if let Some(endpoint) = &host.endpoint {
                        let mut options = McpHttpOptions::new(endpoint);
                        options.bearer_token = Some(auth(&case.auth));
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

fn auth(auth: &OAuthContext) -> String {
    serde_json::to_string(auth).expect("fixture claims serialise")
}
async fn cli_once(
    process: &Process,
    auth_value: &OAuthContext,
    operation: &str,
    input: &Value,
) -> Result<(Vec<Frame>, i32), Failure> {
    let mut child = process
        .command()
        .env("CLAPHAM_FIXTURE_ACCESS", auth(auth_value))
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
    let (described, exit) = cli_once(process, &case.auth, "clapham.describe", &json!({})).await?;
    if exit != 0 {
        return Err(Failure::new("CLI describe was refused"));
    }
    let [Frame::CliResult(value)] = described.as_slice() else {
        return Err(Failure::new("CLI describe returned unexpected frames"));
    };
    let catalogue = names(value)?;
    let (frames, exit_code) = cli_once(process, &case.auth, &case.operation, &case.input).await?;
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
enum HostReply {
    Result(Value),
    Refusal(Refusal),
}
impl HostReply {
    fn result(self) -> Result<Value, Failure> {
        match self {
            Self::Result(value) => Ok(value),
            Self::Refusal(refusal) => Err(Failure::new(format!(
                "HTTP authorisation refused: {refusal}"
            ))),
        }
    }
}
impl Host {
    async fn start(
        process: &Process,
        kind: ProcessKind,
        caller: &OAuthContext,
    ) -> Result<Self, Failure> {
        let http = matches!(kind, ProcessKind::McpHttp | ProcessKind::BoundHttp);
        let mut command = process.command();
        command.arg("serve");
        if http {
            command.args(["--http", "127.0.0.1:0"]);
        } else {
            command
                .args(["--stdio"])
                .env("CLAPHAM_FIXTURE_ACCESS", auth(caller));
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
    async fn tools(
        &mut self,
        caller: &OAuthContext,
        bounds: &Bounds,
    ) -> Result<Vec<String>, Failure> {
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = BTreeSet::new();
        for _ in 0..bounds.pages {
            let page = self
                .request("tools/list", json!({"cursor":cursor}), caller)
                .await?;
            let page = page.result()?;
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
        caller: &OAuthContext,
    ) -> Result<HostReply, Failure> {
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
                .bearer_auth(auth(caller))
                .header("mcp-protocol-version", "2026-07-28")
                .header("mcp-method", method)
                .header("accept", "application/json, text/event-stream");
            if let Some(name) = params.get("name").and_then(Value::as_str) {
                send = send.header("mcp-name", name);
            }
            let response = send
                .json(&request)
                .send()
                .await
                .map_err(|e| Failure::new(e.to_string()))?;
            if matches!(response.status().as_u16(), 401 | 403) {
                // Compare the OAuth HTTP refusal with the same shared boundary on stdio.
                let refusal = response
                    .json::<Refusal>()
                    .await
                    .map_err(|e| Failure::new(e.to_string()))?;
                return Ok(HostReply::Refusal(refusal));
            }
            response
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
            .map(HostReply::Result)
            .ok_or_else(|| Failure::new("MCP response has no result"))
    }
    /// Call a tool under the records extension, acknowledging each record as it arrives.
    async fn call(
        &mut self,
        name: &str,
        input: &Value,
        caller: &OAuthContext,
    ) -> Result<(Vec<Value>, HostReply), Failure> {
        self.id += 1;
        let id = self.id;
        let token = format!("conformance-{id}");
        let request = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":name,"arguments":input,"_meta":records_meta(Some(&token))}});
        let mut records = Vec::new();
        let response = if let Some(endpoint) = self.endpoint.clone() {
            let response = self
                .post(&endpoint, &request, caller)
                .send()
                .await
                .map_err(|e| Failure::new(e.to_string()))?;
            if matches!(response.status().as_u16(), 401 | 403) {
                let refusal = response
                    .json::<Refusal>()
                    .await
                    .map_err(|e| Failure::new(e.to_string()))?;
                return Ok((records, HostReply::Refusal(refusal)));
            }
            let mut response = response
                .error_for_status()
                .map_err(|e| Failure::new(e.to_string()))?;
            let streams = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("text/event-stream"));
            if !streams {
                response
                    .json()
                    .await
                    .map_err(|e| Failure::new(e.to_string()))?
            } else {
                let mut buffer = Vec::new();
                loop {
                    let message = next_event(&mut response, &mut buffer)
                        .await?
                        .ok_or_else(|| Failure::new("SSE reply ended before its result"))?;
                    if message.get("method").is_none() {
                        break message;
                    }
                    if let Some(sequence) = record(&message, &token, &mut records)? {
                        self.id += 1;
                        let acknowledgement = acknowledgement(self.id, &token, sequence);
                        let reply: Value = self
                            .post(&endpoint, &acknowledgement, caller)
                            .send()
                            .await
                            .and_then(reqwest::Response::error_for_status)
                            .map_err(|e| Failure::new(e.to_string()))?
                            .json()
                            .await
                            .map_err(|e| Failure::new(e.to_string()))?;
                        if reply.get("result").is_none() {
                            return Err(Failure::new(format!("acknowledgement refused: {reply}")));
                        }
                    }
                }
            }
        } else {
            self.write(&request).await?;
            // An acknowledgement's reply may follow the result it released.
            let mut acknowledging = BTreeSet::new();
            let mut result = None;
            while result.is_none() || !acknowledging.is_empty() {
                let message = self.read().await?;
                if message.get("method").is_some() {
                    if let Some(sequence) = record(&message, &token, &mut records)? {
                        self.id += 1;
                        acknowledging.insert(self.id);
                        self.write(&acknowledgement(self.id, &token, sequence))
                            .await?;
                    }
                } else if message["id"] == id {
                    result = Some(message);
                } else if let Some(acknowledged) = message["id"].as_u64()
                    && acknowledging.remove(&acknowledged)
                {
                    if message.get("result").is_none() {
                        return Err(Failure::new(format!("acknowledgement refused: {message}")));
                    }
                } else {
                    return Err(Failure::new(format!("unexpected MCP message: {message}")));
                }
            }
            result.expect("loop ends with a result")
        };
        if response["jsonrpc"] != "2.0" || response["id"] != id || response.get("error").is_some() {
            return Err(Failure::new(format!("unexpected MCP response: {response}")));
        }
        let result = response
            .get("result")
            .cloned()
            .ok_or_else(|| Failure::new("MCP response has no result"))?;
        Ok((records, HostReply::Result(result)))
    }
    fn post(
        &self,
        endpoint: &str,
        message: &Value,
        caller: &OAuthContext,
    ) -> reqwest::RequestBuilder {
        let mut send = self
            .client
            .as_ref()
            .unwrap()
            .post(endpoint)
            .bearer_auth(auth(caller))
            .header("mcp-protocol-version", "2026-07-28")
            .header("mcp-method", message["method"].as_str().unwrap_or_default())
            .header("accept", "application/json, text/event-stream");
        if let Some(name) = message["params"].get("name").and_then(Value::as_str) {
            send = send.header("mcp-name", name);
        }
        send.json(message)
    }
    async fn write(&mut self, message: &Value) -> Result<(), Failure> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| Failure::new("missing host stdin"))?;
        input.write_all(format!("{message}\n").as_bytes()).await?;
        input.flush().await?;
        Ok(())
    }
    async fn read(&mut self) -> Result<Value, Failure> {
        let mut line = String::new();
        if self.output.as_mut().unwrap().read_line(&mut line).await? == 0 {
            return Err(Failure::new("MCP host closed before its response"));
        }
        Ok(serde_json::from_str(&line)?)
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
fn records_meta(token: Option<&str>) -> Value {
    let mut meta = json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{"extensions":{RECORDS_EXTENSION:{}}}});
    if let Some(token) = token {
        meta[RECORDS_EXTENSION] = json!(token);
    }
    meta
}
fn acknowledgement(id: u64, token: &str, sequence: u64) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":ACKNOWLEDGE_REQUEST,
        "params":{"token":token,"sequence":sequence,"_meta":records_meta(None)}})
}
/// The next record's sequence, after checking its token and order; `None` for any other
/// notification.
fn record(message: &Value, token: &str, records: &mut Vec<Value>) -> Result<Option<u64>, Failure> {
    if !notification(message) || message["method"] != RECORD_NOTIFICATION {
        return Ok(None);
    }
    let params = &message["params"];
    let sequence = records.len() as u64 + 1;
    if params["token"] != token || params["sequence"] != sequence || params.get("record").is_none()
    {
        return Err(Failure::new(format!("record out of order: {message}")));
    }
    records.push(params["record"].clone());
    Ok(Some(sequence))
}
/// The next JSON-RPC message in an SSE reply, or `None` when the reply ends.
async fn next_event(
    response: &mut reqwest::Response,
    buffer: &mut Vec<u8>,
) -> Result<Option<Value>, Failure> {
    loop {
        if let Some(end) = buffer.windows(2).position(|pair| pair == b"\n\n") {
            let event: Vec<u8> = buffer.drain(..end + 2).collect();
            let event =
                std::str::from_utf8(&event).map_err(|_| Failure::new("SSE event is not UTF-8"))?;
            let data: Vec<&str> = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|data| data.strip_prefix(' ').unwrap_or(data))
                .collect();
            if !data.is_empty() {
                return Ok(Some(serde_json::from_str(&data.join("\n"))?));
            }
            continue;
        }
        let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| Failure::new(e.to_string()))?
        else {
            return Ok(None);
        };
        buffer.extend_from_slice(&chunk);
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
    describe_selection.scopes = case.auth.scopes().iter().cloned().collect();
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
            case.auth.clone(),
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
    let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let records = {
        let frames = frames.clone();
        RecordSink::new(move |record| {
            frames.lock().unwrap().push(Frame::Record(record));
            async { Ok(()) }
        })
    };
    let mut selection = McpSelection::new(&case.operation);
    selection.alias = Some("call".into());
    selection.scopes = case.auth.scopes().iter().cloned().collect();
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
                case.auth.clone(),
                "remote.call",
                case.input.clone(),
                records,
                Cancellation::new(),
                false,
            )
            .await
        }
    };
    let mut frames = std::mem::take(&mut *frames.lock().unwrap());
    frames.push(terminal(result));
    Ok(Observation {
        catalogue: names,
        frames,
        exit_code: None,
    })
}
