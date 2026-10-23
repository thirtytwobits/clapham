//! Exercise the MCP wire through an actual child process's stdin/stdout.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

struct Client {
    process: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    id: u64,
}
impl Client {
    fn new(agent: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_clapham-mcp-fixture"));
        if agent {
            command.arg("agent");
        }
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = process.stdin.take();
        let output = BufReader::new(process.stdout.take().unwrap());
        Self {
            process,
            input,
            output,
            id: 0,
        }
    }
    fn request(&mut self, method: &str, mut params: Value) -> Value {
        let meta = params
            .as_object_mut()
            .unwrap()
            .entry("_meta")
            .or_insert(json!({}));
        meta["io.modelcontextprotocol/protocolVersion"] = json!("2026-07-28");
        meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
        meta["io.modelcontextprotocol/clientInfo"] = json!({"name":"lists-test","version":"1"});
        self.id += 1;
        writeln!(
            self.input.as_mut().unwrap(),
            "{}",
            json!({"jsonrpc":"2.0","id":self.id,"method":method,"params":params})
        )
        .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        let mut line = String::new();
        assert_ne!(
            self.output.read_line(&mut line).unwrap(),
            0,
            "server closed"
        );
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], self.id);
        response
    }
    fn result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        drop(self.input.take());
        // EOF closes the transport; no sleeps or deadline-based polling.
        assert!(self.process.wait().unwrap().success());
    }
}

#[test]
fn stdio_lists_effects_calls_refusals_resources_and_preview() {
    let mut client = Client::new(false);
    let discovery = client.result("server/discover", json!({}));
    assert_eq!(discovery["supportedVersions"], json!(["2026-07-28"]));
    assert!(discovery["capabilities"]["tools"].is_object());
    assert_eq!(discovery["cacheScope"], "private");
    let catalogue = client.result("tools/list", json!({}));
    assert_eq!(catalogue["ttlMs"], 0);
    assert_eq!(catalogue["cacheScope"], "private");
    let tools = catalogue["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 14);
    for (name, readonly, destructive, idempotent) in [
        ("lists.get", true, false, true),
        ("lists.create", false, false, false),
        ("lists.compact", false, false, true),
        ("lists.delete", false, true, false),
    ] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert_eq!(tool["annotations"]["readOnlyHint"], readonly, "{name}");
        assert_eq!(
            tool["annotations"]["destructiveHint"], destructive,
            "{name}"
        );
        assert_eq!(tool["annotations"]["idempotentHint"], idempotent, "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], false);
        assert!(tool["inputSchema"].is_object());
    }
    let compact = tools
        .iter()
        .find(|tool| tool["name"] == "lists.compact")
        .unwrap();
    assert_eq!(
        compact["_meta"]["io.github.thirtytwobits.clapham/effect"],
        "administrative"
    );
    let got = client.result(
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"tasks"}}),
    );
    assert_eq!(got["structuredContent"]["name"], "Tasks");
    assert_eq!(got["isError"], false);
    let missing = client.result(
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"missing"}}),
    );
    assert_eq!(missing["isError"], true);
    assert_eq!(missing["structuredContent"]["code"], "list_not_found");
    assert_eq!(missing["structuredContent"]["category"], "not_found");
    let templates = client.result("resources/templates/list", json!({}));
    assert!(
        templates["resourceTemplates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["uriTemplate"] == "lists://{id}")
    );
    let resource = client.result("resources/read", json!({"uri":"lists://tasks"}));
    assert_eq!(
        serde_json::from_str::<Value>(resource["contents"][0]["text"].as_str().unwrap()).unwrap(),
        got["structuredContent"]
    );
    let encoded = client.result("resources/read", json!({"uri":"lists://a%2Fb"}));
    assert_eq!(
        serde_json::from_str::<Value>(encoded["contents"][0]["text"].as_str().unwrap()).unwrap()["name"],
        "Encoded"
    );
    let preview = client.result("tools/call", json!({"name":"lists.create","arguments":{"list":{"id":"preview","name":"Preview"}},"_meta":{"io.github.thirtytwobits.clapham/preview":true}}));
    assert_eq!(preview["isError"], false);
    let missing = client.result(
        "tools/call",
        json!({"name":"lists.get","arguments":{"id":"preview"}}),
    );
    assert_eq!(missing["structuredContent"]["code"], "list_not_found");
    let exported = client.result(
        "tools/call",
        json!({"name":"lists.export","arguments":{"id":"tasks"}}),
    );
    assert_eq!(exported["structuredContent"]["count"], 1);
}

#[test]
fn agent_discovery_and_calls_use_the_launchers_principal() {
    let mut client = Client::new(true);
    let catalogue = client.result("tools/list", json!({}));
    assert!(
        !catalogue["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "lists.compact")
    );
    let denied = client.result("tools/call", json!({"name":"lists.compact","arguments":{},"_meta":{"principal":{"id":"human","kind":"human"}}}));
    assert_eq!(denied["structuredContent"]["code"], "not_admitted");
    let invalid = client.result("tools/call", json!({"name":"lists.get","arguments":{}}));
    assert_eq!(invalid["structuredContent"]["code"], "invalid_input");
    let protocol = client.request(
        "tools/call",
        json!({"name":"lists.get","_meta":{"io.github.thirtytwobits.clapham/preview":"yes"}}),
    );
    assert_eq!(protocol["error"]["code"], -32602);
}
