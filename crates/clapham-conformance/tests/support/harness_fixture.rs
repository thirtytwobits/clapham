//! Scripted stdio peer for testing discovery leaks, pagination and notifications.

use clapham::{Principal, PrincipalKind};
use clapham_conformance::{Case, Frame, FrontEnd, InProcess, lists_fixture, lists_gate};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let flag = |name| args.iter().any(|arg| arg == name);
    let identity = args
        .windows(2)
        .find(|pair| pair[0] == "--as")
        .map(|pair| pair[1].as_str())
        .unwrap();
    let (kind, id) = identity.split_once(':').unwrap();
    let caller = Principal::new(
        id,
        match kind {
            "human" => PrincipalKind::Human,
            "agent" => PrincipalKind::Agent,
            "service" => PrincipalKind::Service,
            "system" => PrincipalKind::System,
            _ => unreachable!(),
        },
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut output = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let mut case = Case::new("lists.get", json!({"id":"tasks"}));
        case.principal = caller.clone();
        if request["method"] == "tools/call" && request["params"]["name"] != "clapham.describe" {
            case.operation = request["params"]["name"].as_str().unwrap().to_owned();
            case.input = request["params"]["arguments"].clone();
        }
        let observed = runtime
            .block_on(InProcess.observe(&lists_fixture(), lists_gate(), &case))
            .unwrap();
        let descriptors: Vec<_> = observed
            .catalogue
            .iter()
            .map(|name| json!({"name":name}))
            .collect();
        let result = if request["method"] == "tools/list" {
            let mut tools = descriptors;
            if flag("--leak") {
                // Describe remains correct while tools/list leaks a denied operation.
                tools.push(json!({"name":"lists.get"}));
            }
            if flag("--duplicate") {
                tools.push(tools[0].clone());
            }
            let start: usize = request["params"]["cursor"]
                .as_str()
                .map(|cursor| cursor.parse().unwrap())
                .unwrap_or(0);
            let end = (start + 2).min(tools.len());
            let mut page = json!({"tools":tools[start..end]});
            if flag("--repeated-cursor") {
                page["nextCursor"] = json!("0");
            } else if end < tools.len() {
                page["nextCursor"] = json!(end.to_string());
            }
            page
        } else if request["params"]["name"] == "clapham.describe" {
            json!({"content":[],"structuredContent":descriptors})
        } else {
            match observed.frames.last().unwrap() {
                Frame::Outcome(outcome) => json!({"content":[],"structuredContent":outcome.result,
                    "isError":outcome.is_error}),
                Frame::Refusal(refusal) => {
                    json!({"content":[],"structuredContent":refusal,"isError":true})
                }
                _ => unreachable!(),
            }
        };
        if flag("--notifications") {
            writeln!(
                output,
                "{}",
                json!({"jsonrpc":"2.0","method":"notifications/message",
                "params":{"level":"info","data":"scripted progress"}})
            )
            .unwrap();
        }
        if flag("--malformed-notification") {
            writeln!(output, "{}", json!({"jsonrpc":"2.0","result":{}})).unwrap();
        }
        let id = if flag("--wrong-id") {
            json!(999)
        } else {
            request["id"].clone()
        };
        let response = json!({"jsonrpc":"2.0","id":id,"result":result});
        writeln!(output, "{response}").unwrap();
        output.flush().unwrap();
    }
    if flag("--notifications") {
        writeln!(
            output,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/message",
            "params":{"level":"info","data":"finished"}})
        )
        .unwrap();
    }
    if flag("--extra-result") {
        writeln!(output, "{}", json!({"jsonrpc":"2.0","id":999,"result":{}})).unwrap();
    }
}
