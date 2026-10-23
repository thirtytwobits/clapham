//! Drive the descriptor-built CLI against the shared lists-and-items fixture.

use std::io;
use std::sync::Arc;

use clapham::{
    Bounds, Cancellation, Category, ComposeOptions, Evidence, Principal, PrincipalKind, RecordSink,
    compose_with_options,
};
use clapham_cli::{Cli, CliError};
use clapham_conformance::{ListsGate, ListsHandle};
use futures_executor::block_on;
use serde_json::{Value, json};

fn fixture() -> (ListsHandle, Cli) {
    let handle = ListsHandle::new();
    let set = compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    (
        handle,
        Cli::new(
            "lists-app",
            set,
            Principal::new("operator", PrincipalKind::Human),
        )
        .unwrap(),
    )
}

fn run(cli: &Cli, args: &[&str], stdin: &[u8]) -> Result<Value, CliError> {
    block_on(cli.run_from(args, stdin, RecordSink::default(), Cancellation::new()))
        .map(|outcome| outcome.result.unwrap())
}

#[test]
fn create_json_and_get_positional_run_through_the_generated_tree() {
    let (handle, cli) = fixture();
    let created = run(
        &cli,
        &[
            "lists-app",
            "lists",
            "create",
            "--json",
            r#"{"list":{"id":"tasks","name":"Tasks"}}"#,
        ],
        &[],
    )
    .unwrap();
    let got = run(&cli, &["lists-app", "lists", "get", "tasks"], &[]).unwrap();
    assert_eq!(got, created);
    assert_eq!(got["name"], "Tasks");
    assert_eq!(handle.snapshot().len(), 1);
    assert_eq!(
        run(&cli, &["lists-app", "lists", "list"], &[]).unwrap(),
        json!([got])
    );
}

#[test]
fn object_flags_and_whole_json_stdin_share_the_same_binding() {
    let (handle, cli) = fixture();
    run(
        &cli,
        &[
            "lists-app",
            "lists",
            "create",
            "--list-json",
            r#"{"id":"tasks","name":"Tasks"}"#,
        ],
        &[],
    )
    .unwrap();
    run(
        &cli,
        &[
            "lists-app",
            "items",
            "add",
            "--list_id",
            "tasks",
            "--item-json",
            r#"{"id":"a","text":"Read"}"#,
        ],
        &[],
    )
    .unwrap();
    let got = run(
        &cli,
        &["lists-app", "lists", "get", "--json", "-"],
        br#"{"id":"tasks"}"#,
    )
    .unwrap();
    assert_eq!(got["items"][0]["text"], "Read");
    assert_eq!(handle.get("tasks").unwrap().items.len(), 1);
}

#[test]
fn help_of_each_operation_contains_its_descriptor_and_effect() {
    let (_, cli) = fixture();
    let mut command = cli.command();
    command.build();
    for (namespace, leaf, effect, description) in [
        (
            "lists",
            "get",
            "query",
            "Read one list, including its revision and items.",
        ),
        ("lists", "create", "mutation", "Create a list"),
        ("lists", "compact", "administrative", "Remove ticked items"),
        ("lists", "delete", "destructive", "Delete a list"),
    ] {
        let error = cli
            .parse_from(["lists-app", namespace, leaf, "--help"], io::empty())
            .unwrap_err();
        assert_eq!(error.exit_code(), 0);
        let CliError::Help(help) = error else {
            panic!("expected help")
        };
        assert!(help.contains(description), "{help}");
        assert!(help.contains(&format!("Effect: {effect}")), "{help}");
    }
}

#[test]
fn binding_errors_are_usage_and_never_mutate_the_fixture() {
    let (handle, cli) = fixture();
    for args in [
        vec!["lists-app", "lists", "create"],
        vec![
            "lists-app",
            "lists",
            "create",
            "--json",
            r#"{"list":{"id":"tasks","name":"Tasks"}}"#,
            "--list-json",
            "{}",
        ],
        vec![
            "lists-app",
            "lists",
            "rename",
            "--id",
            "tasks",
            "--name",
            "New",
            "--revision",
            "false",
        ],
        vec![
            "lists-app",
            "lists",
            "rename",
            "--id",
            "tasks",
            "--name",
            "New",
            "--revision",
            "1.5",
        ],
    ] {
        let error = run(&cli, &args, &[]).unwrap_err();
        assert_eq!(error.exit_code(), 2, "{error}");
        let CliError::Refusal(refusal) = error else {
            panic!("expected refusal")
        };
        assert_eq!(
            (refusal.code.as_str(), refusal.category, refusal.evidence),
            ("usage", Category::Usage, Evidence::NotDispatched)
        );
        assert!(handle.snapshot().is_empty());
    }
}

#[test]
fn invalid_documents_keep_invalid_input_and_never_mutate_the_fixture() {
    let (handle, cli) = fixture();
    for args in [
        vec!["lists-app", "lists", "create", "--json", r#"{"list":12}"#],
        vec!["lists-app", "lists", "create", "--list-json", "[1]"],
        vec![
            "lists-app",
            "lists",
            "create",
            "--json",
            r#"{"list":{"id":"tasks","name":"Tasks"},"extra":true}"#,
        ],
        vec!["lists-app", "lists", "create", "--json", "{}"],
    ] {
        let error = run(&cli, &args, &[]).unwrap_err();
        assert_eq!(error.exit_code(), 65);
        let CliError::Refusal(refusal) = error else {
            panic!("expected refusal")
        };
        assert_eq!(
            (refusal.code.as_str(), refusal.category, refusal.evidence),
            (
                "invalid_input",
                Category::InvalidInput,
                Evidence::NotDispatched
            )
        );
        assert!(handle.snapshot().is_empty());
    }
}

#[test]
fn as_agent_is_denied_by_both_descriptor_defaults_and_the_lists_gate() {
    let (handle, cli) = fixture();
    handle.create_list("tasks", "Tasks").unwrap();
    handle.add_item("tasks", "a", "Read").unwrap();
    let before = handle.snapshot();
    for args in [
        vec![
            "lists-app",
            "--as",
            "agent:bot",
            "lists",
            "compact",
            "--yes",
        ],
        vec![
            "lists-app",
            "items",
            "tick",
            "--list_id",
            "tasks",
            "--id",
            "a",
            "--as",
            "agent:bot",
        ],
    ] {
        let error = run(&cli, &args, &[]).unwrap_err();
        assert_eq!(error.exit_code(), 77);
        let CliError::Refusal(refusal) = error else {
            panic!("expected refusal")
        };
        assert_eq!(refusal.code, "not_admitted");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
    }
    assert_eq!(handle.snapshot(), before);
}

#[test]
fn describe_alias_uses_the_same_principal_filtered_catalogue() {
    let (_, cli) = fixture();
    let args = ["lists-app", "--as", "agent:bot", "describe"];
    let alias = run(&cli, &args, &[]).unwrap();
    let full = run(
        &cli,
        &["lists-app", "--as", "agent:bot", "clapham", "describe"],
        &[],
    )
    .unwrap();
    assert_eq!(alias, full);
    let names: Vec<_> = alias
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"lists.get"));
    assert!(!names.contains(&"lists.compact"));
    assert!(!names.contains(&"items.tick"));
}

#[test]
fn records_and_domain_refusals_retain_the_call_runners_contract() {
    let (handle, cli) = fixture();
    handle.create_list("tasks", "Tasks").unwrap();
    handle.add_item("tasks", "a", "Read").unwrap();
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let delivered = records.clone();
    let outcome = block_on(cli.run_from(
        ["lists-app", "lists", "export", "--id", "tasks"],
        io::empty(),
        RecordSink::new(move |record| {
            delivered.lock().unwrap().push(record);
            async { Ok(()) }
        }),
        Cancellation::new(),
    ))
    .unwrap();
    assert_eq!(outcome.result, Some(json!({"count":1})));
    assert_eq!(
        records.lock().unwrap().as_slice(),
        &[json!({"id":"a","text":"Read","ticked":false})]
    );
    for (args, code, exit) in [
        (
            vec!["lists-app", "lists", "get", "absent"],
            "list_not_found",
            66,
        ),
        (
            vec!["lists-app", "lists", "fault"],
            "undeclared_refusal",
            70,
        ),
    ] {
        let error = run(&cli, &args, &[]).unwrap_err();
        assert_eq!(error.exit_code(), exit);
        let CliError::Refusal(refusal) = error else {
            panic!("expected refusal")
        };
        assert_eq!(refusal.code, code);
        assert_eq!(refusal.evidence, Evidence::Completed);
    }
}
