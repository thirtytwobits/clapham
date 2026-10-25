//! Regression checks against a scripted peer, independent of the real MCP server.

use clapham_conformance::*;
use serde_json::json;

fn driver(flags: &[&str]) -> ProcessFrontEnd {
    let mut process = Process::new(env!("CARGO_BIN_EXE_clapham-harness-fixture"));
    process.args = flags.iter().map(Into::into).collect();
    ProcessFrontEnd::new(ProcessKind::McpStdio, process)
}

#[tokio::test]
async fn tools_list_leaks_and_cross_page_duplicates_fail_even_when_describe_is_correct() {
    let fixture = lists_fixture();
    let mut case = Case::new("lists.get", json!({"id":"tasks"}));
    case.auth = fixture.denied_auth.clone();
    for flag in ["--leak", "--duplicate"] {
        let error = driver(&[flag])
            .observe(&fixture, lists_gate(), &case)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("tools/list catalogue"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn stdio_notifications_do_not_hide_wrong_responses_or_extra_terminals() {
    let fixture = lists_fixture();
    let case = Case::new("lists.get", json!({"id":"tasks"}));
    let observed = driver(&["--notifications"])
        .observe(&fixture, lists_gate(), &case)
        .await
        .unwrap();
    let expected = InProcess
        .observe(&fixture, lists_gate(), &case)
        .await
        .unwrap();
    assert_eq!(observed.catalogue, expected.catalogue);
    let [Frame::Outcome(actual)] = observed.frames.as_slice() else {
        panic!("expected one terminal outcome");
    };
    let [Frame::Outcome(expected)] = expected.frames.as_slice() else {
        panic!("expected one baseline outcome");
    };
    assert_eq!(actual.result, expected.result);
    for flag in ["--wrong-id", "--malformed-notification"] {
        let error = driver(&["--notifications", flag])
            .observe(&fixture, lists_gate(), &case)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("unexpected MCP response"),
            "{error}"
        );
    }
    let error = driver(&["--notifications", "--extra-result"])
        .observe(&fixture, lists_gate(), &case)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("extra result"), "{error}");
    let error = driver(&["--repeated-cursor"])
        .observe(&fixture, lists_gate(), &case)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("repeated tools/list cursor"),
        "{error}"
    );
}
