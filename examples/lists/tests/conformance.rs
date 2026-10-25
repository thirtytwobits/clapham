//! The reusable suite drives the actual reference executable through all seven front ends.

use clapham_conformance::{
    FrontEnd, InProcess, Process, ProcessFrontEnd, ProcessKind, lists_fixture, lists_gate, run,
};

#[tokio::test]
async fn lists_passes_every_front_end_as_real_processes() {
    let mut fixture = lists_fixture();
    // The same identity has different admission as a human and an agent. HTTP must
    // authenticate the full auth, rather than silently mapping its id to a kind.
    let mut human_tick = fixture
        .cases
        .iter()
        .find(|case| case.auth.subject() == "agent")
        .unwrap()
        .clone();
    human_tick.auth = clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        "agent",
        ["lists:read", "lists:write", "lists:admin", "items:tick"],
    );
    fixture.cases.push(human_tick);
    let process = Process::new(env!("CARGO_BIN_EXE_lists"));
    let mut front_ends: Vec<Box<dyn FrontEnd>> = vec![Box::new(InProcess)];
    for kind in [
        ProcessKind::CommandLine,
        ProcessKind::CommandLineStdio,
        ProcessKind::McpStdio,
        ProcessKind::McpHttp,
        ProcessKind::BoundStdio,
        ProcessKind::BoundHttp,
    ] {
        front_ends.push(Box::new(ProcessFrontEnd::new(kind, process.clone())));
    }
    let report = run(&fixture, lists_gate(), front_ends).await.unwrap();
    assert_eq!(report.front_ends, 7);
    assert_eq!(report.calls, fixture.cases.len() * 7);
    assert_eq!(report.gate_checks, 12 * 7);
    assert_eq!(report.namespace_checks, 2);
}
