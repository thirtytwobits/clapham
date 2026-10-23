//! The reusable suite drives the actual reference executable through all six front ends.

use clapham_conformance::{
    FrontEnd, InProcess, Process, ProcessFrontEnd, ProcessKind, lists_fixture, lists_gate, run,
};

#[tokio::test]
async fn lists_passes_every_front_end_as_real_processes() {
    let fixture = lists_fixture();
    let process = Process::new(env!("CARGO_BIN_EXE_lists"));
    let mut front_ends: Vec<Box<dyn FrontEnd>> = vec![Box::new(InProcess)];
    for kind in [
        ProcessKind::CommandLine,
        ProcessKind::McpStdio,
        ProcessKind::McpHttp,
        ProcessKind::BoundStdio,
        ProcessKind::BoundHttp,
    ] {
        front_ends.push(Box::new(ProcessFrontEnd::new(kind, process.clone())));
    }
    let report = run(&fixture, lists_gate(), front_ends).await.unwrap();
    assert_eq!(report.front_ends, 6);
    assert_eq!(report.calls, fixture.cases.len() * 6);
    assert_eq!(report.gate_checks, 12 * 6);
    assert_eq!(report.namespace_checks, 2);
}
