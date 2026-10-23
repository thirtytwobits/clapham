//! Command generation, input binding and dispatch boundaries.

use std::io::{self, Read};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use clapham::{
    Bounds, Call, Cancellation, ComposeOptions, Contribution, Evidence, ExecutionMode, Operation,
    Outcome, Principal, PrincipalKind, RecordSink, compose_with_options,
};
use clapham_cli::{Cli, CliError};
use futures_executor::block_on;
use serde_json::{Value, json};

fn descriptor(input: Value) -> Operation {
    serde_json::from_value(json!({
        "name":"lists.document.edit", "title":"Edit a document", "description":"Apply the supplied document fields.",
        "input":input, "effect":"mutation", "locality":"workspace", "idempotent":false, "open_world":false
    })).unwrap()
}

fn fixture(operation: Operation, mode: ExecutionMode, bounds: Bounds) -> (Cli, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let set = compose_with_options(
        [
            Contribution::new("lists").operation(operation, move |call: Call| {
                count.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok(Outcome::new(
                        json!({"input":call.input,"principal":call.principal}),
                    ))
                }
            }),
        ],
        bounds,
        ComposeOptions {
            execution: mode,
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    (
        Cli::new("test", set, Principal::new("account", PrincipalKind::Human)).unwrap(),
        calls,
    )
}

fn run(cli: &Cli, args: &[&str]) -> Result<Value, CliError> {
    block_on(cli.run_from(
        args,
        io::empty(),
        RecordSink::default(),
        Cancellation::new(),
    ))
    .map(|outcome| outcome.result.unwrap())
}

struct Unread;
impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("stdin must not be read")
    }
}

#[test]
fn binds_scalar_types_enums_and_json_properties_without_coercion() {
    let (cli, calls) = fixture(
        descriptor(json!({
            "type":"object", "properties":{
                "text":{"type":"string"}, "integer":{"type":"integer"}, "number":{"type":"number"},
                "enabled":{"type":"boolean"}, "mode":{"type":"string","enum":["brief","full"]},
                "values":{"type":"array","items":{"type":"integer"}}, "object":{"type":"object"}
            }, "required":["text","integer","number","enabled","mode","values","object"], "additionalProperties":false
        })),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    let outcome = run(
        &cli,
        &[
            "test",
            "lists",
            "document",
            "edit",
            "--text",
            "12",
            "--integer",
            "-7",
            "--number",
            "1.5",
            "--enabled",
            "false",
            "--mode",
            "full",
            "--values-json",
            "[1,2]",
            "--object-json",
            "{}",
        ],
    )
    .unwrap();
    assert_eq!(
        outcome["input"],
        json!({"text":"12","integer":-7,"number":1.5,"enabled":false,"mode":"full","values":[1,2],"object":{}})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn scalar_conversion_and_missing_arguments_refuse_as_usage() {
    for (schema, value) in [
        (json!({"type":"integer"}), "no"),
        (json!({"type":"integer"}), "1.5"),
        (json!({"type":"number"}), "true"),
        (json!({"type":"number"}), "NaN"),
        (json!({"type":"boolean"}), "1"),
    ] {
        let (cli, calls) = fixture(
            descriptor(json!({"type":"object","properties":{"value":schema},"required":["value"]})),
            ExecutionMode::InProcess,
            Bounds::default(),
        );
        for args in [
            vec!["test", "lists", "document", "edit", "--value", value],
            vec!["test", "lists", "document", "edit"],
        ] {
            let error = run(&cli, &args).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{error}");
            let CliError::Refusal(refusal) = error else {
                panic!("expected refusal")
            };
            assert_eq!(refusal.code, "usage");
            assert_eq!(refusal.evidence, Evidence::NotDispatched);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn positionals_keep_descriptor_order_instead_of_schema_key_order() {
    let mut operation = descriptor(
        json!({"type":"object", "properties":{"a":{"type":"integer"},"z":{"type":"string"}},"required":["z","a"]}),
    );
    operation.positional = vec!["z".into(), "a".into()];
    let (cli, _) = fixture(operation, ExecutionMode::InProcess, Bounds::default());
    let parsed = cli
        .parse_from(
            ["test", "lists", "document", "edit", "last-key", "42"],
            Unread,
        )
        .unwrap();
    assert_eq!(parsed.input, json!({"z":"last-key","a":42}));
    assert_eq!(
        cli.parse_from(
            [
                "test",
                "lists",
                "document",
                "edit",
                "--json",
                r#"{"z":"first","a":2}"#
            ],
            Unread
        )
        .unwrap()
        .input,
        json!({"z":"first","a":2})
    );
}

#[test]
fn help_uses_title_description_property_descriptions_and_effect() {
    let (cli, calls) = fixture(
        descriptor(
            json!({"type":"object","properties":{"value":{"type":"string","description":"Text to insert."}}}),
        ),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    for flag in ["--help", "-h"] {
        let error = cli
            .parse_from(["test", "lists", "document", "edit", flag], Unread)
            .unwrap_err();
        let CliError::Help(text) = error else {
            panic!("expected help")
        };
        for expected in [
            "Edit a document",
            "Apply the supplied document fields.",
            "Text to insert.",
            "Effect: mutation",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    cli.command().debug_assert();
}

#[test]
fn process_principal_and_explicit_as_are_preserved_in_handler_context() {
    let (cli, _) = fixture(
        descriptor(json!({"type":"object"})),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    assert_eq!(
        run(&cli, &["test", "lists", "document", "edit"]).unwrap()["principal"],
        json!({"id":"account","kind":"human"})
    );
    for kind in ["human", "agent", "service", "system"] {
        let identity = format!("{kind}:explicit:identity");
        let outcome = run(
            &cli,
            &["test", "lists", "document", "edit", "--as", &identity],
        )
        .unwrap();
        assert_eq!(
            outcome["principal"],
            json!({"id":"explicit:identity","kind":kind})
        );
    }
    let principal = clapham_cli::process_principal().unwrap();
    assert_eq!(principal.kind, PrincipalKind::Human);
    assert!(!principal.id.is_empty());
}

#[test]
fn malformed_as_is_usage_and_does_not_dispatch() {
    let (cli, calls) = fixture(
        descriptor(json!({"type":"object"})),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    for identity in ["agent", "agent:", ":bot", "unknown:bot"] {
        assert_eq!(
            run(
                &cli,
                &["test", "--as", identity, "lists", "document", "edit"]
            )
            .unwrap_err()
            .exit_code(),
            2
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn daemon_operations_never_run_even_in_a_daemon_composed_set() {
    for mode in [ExecutionMode::InProcess, ExecutionMode::Daemon] {
        let mut operation = descriptor(json!({"type":"object"}));
        operation.locality = clapham::Locality::Daemon;
        let (cli, calls) = fixture(operation, mode, Bounds::default());
        let CliError::Refusal(refusal) =
            run(&cli, &["test", "lists", "document", "edit"]).unwrap_err()
        else {
            panic!("expected refusal")
        };
        assert_eq!(refusal.code, "no_daemon");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn malformed_json_is_usage_but_schema_invalid_json_keeps_invalid_input() {
    let (cli, calls) = fixture(
        descriptor(json!({"type":"object"})),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    for (document, exit) in [("{", 2), ("[]", 65), ("null", 65), ("true", 65)] {
        assert_eq!(
            cli.parse_from(
                ["test", "lists", "document", "edit", "--json", document],
                Unread
            )
            .unwrap_err()
            .exit_code(),
            exit
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("broken input"))
        }
    }
    assert_eq!(
        cli.parse_from(["test", "lists", "document", "edit", "--json", "-"], Broken)
            .unwrap_err()
            .exit_code(),
        2
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn local_refs_and_complex_schemas_are_bound_without_losing_validation() {
    let (cli, _) = fixture(
        descriptor(
            json!({"type":"object", "$defs":{"Count":{"type":"integer"}},"properties":{
        "count":{"$ref":"#/$defs/Count"}, "choice":{"oneOf":[{"type":"string"},{"type":"null"}]}
    },"required":["count","choice"]}),
        ),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    let invocation = cli
        .parse_from(
            [
                "test",
                "lists",
                "document",
                "edit",
                "--count",
                "4",
                "--choice-json",
                "null",
            ],
            Unread,
        )
        .unwrap();
    assert_eq!(invocation.input, json!({"count":4,"choice":null}));
    assert_eq!(
        cli.parse_from(
            [
                "test",
                "lists",
                "document",
                "edit",
                "--count",
                "no",
                "--choice-json",
                "null"
            ],
            Unread
        )
        .unwrap_err()
        .exit_code(),
        2
    );
}

#[test]
fn command_prefixes_can_be_operations_as_well_as_namespaces() {
    let mut parent = descriptor(json!({"type":"object"}));
    parent.name = "lists.document".parse().unwrap();
    let child = descriptor(json!({"type":"object"}));
    let set = clapham::compose(
        [Contribution::new("lists")
            .operation(parent, |_: Call| async {
                Ok(Outcome::new(json!("parent")))
            })
            .operation(child, |_: Call| async { Ok(Outcome::new(json!("child"))) })],
        Bounds::default(),
    )
    .unwrap();
    let cli = Cli::new("test", set, Principal::new("account", PrincipalKind::Human)).unwrap();
    assert_eq!(run(&cli, &["test", "lists", "document"]).unwrap(), "parent");
    assert_eq!(
        run(&cli, &["test", "lists", "document", "edit"]).unwrap(),
        "child"
    );
}

#[test]
fn invalid_bindings_return_configuration_refusals_instead_of_panicking() {
    for (name, schema, positional) in [
        ("lists..edit", json!({"type":"object"}), vec![]),
        (
            "lists.edit",
            json!({"type":"object","properties":{"json":{"type":"string"}}}),
            vec![],
        ),
        (
            "lists.edit",
            json!({"type":"object","properties":{"data":{"type":"object"},"data-json":{"type":"string"}}}),
            vec![],
        ),
        (
            "lists.edit",
            json!({"type":"object"}),
            vec!["absent".into()],
        ),
        (
            "lists.edit",
            json!({"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},"required":["b"]}),
            vec!["a".into(), "b".into()],
        ),
    ] {
        let mut operation = descriptor(schema);
        operation.name = name.parse().unwrap();
        operation.positional = positional;
        let set = clapham::compose(
            [Contribution::new("lists").operation(operation, |_: Call| async { unreachable!() })],
            Bounds::default(),
        )
        .unwrap();
        let refusal =
            Cli::new("test", set, Principal::new("account", PrincipalKind::Human)).unwrap_err();
        assert_eq!(refusal.code, "cli_configuration");
    }
}

#[test]
fn cancellation_and_handler_refusals_keep_the_core_contract() {
    let (cli, calls) = fixture(
        descriptor(json!({"type":"object"})),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    let invocation = cli
        .parse_from(["test", "lists", "document", "edit"], Unread)
        .unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    let refusal = block_on(cli.call(invocation, RecordSink::default(), cancel)).unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::NotDispatched);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn integer_arguments_preserve_unsigned_precision_and_integral_json_numbers() {
    let (cli, _) = fixture(
        descriptor(json!({
            "type":"object", "properties":{"value":{"type":"integer"}}, "required":["value"]
        })),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    let parsed = cli
        .parse_from(
            [
                "test",
                "lists",
                "document",
                "edit",
                "--value",
                "18446744073709551615",
            ],
            Unread,
        )
        .unwrap();
    assert_eq!(parsed.input["value"].as_u64(), Some(u64::MAX));
    let parsed = cli
        .parse_from(
            ["test", "lists", "document", "edit", "--value", "1.0"],
            Unread,
        )
        .unwrap();
    assert_eq!(parsed.input["value"], json!(1.0));
}

#[test]
fn json_cannot_mix_with_positionals_and_unknown_arguments_are_usage() {
    let mut operation = descriptor(
        json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
    );
    operation.positional = vec!["id".into()];
    let (cli, calls) = fixture(operation, ExecutionMode::InProcess, Bounds::default());
    for args in [
        vec![
            "test",
            "lists",
            "document",
            "edit",
            "a",
            "--json",
            r#"{"id":"b"}"#,
        ],
        vec!["test", "lists", "document", "edit", "--unknown"],
        vec!["test", "lists", "document", "absent"],
        vec!["test", "lists", "document", "edit"],
    ] {
        assert_eq!(cli.parse_from(args, Unread).unwrap_err().exit_code(), 2);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn optional_properties_stay_absent_and_whole_json_is_bounded_in_both_forms() {
    let bounds = Bounds {
        message_bytes: 8192,
        schema_bytes: 8192,
        ..Bounds::default()
    };
    let (cli, calls) = fixture(
        descriptor(
            json!({"type":"object","properties":{"value":{"type":"string","default":"example"}}}),
        ),
        ExecutionMode::InProcess,
        bounds,
    );
    assert_eq!(
        cli.parse_from(["test", "lists", "document", "edit"], Unread)
            .unwrap()
            .input,
        json!({})
    );
    let prefix = r#"{"value":""#;
    let suffix = r#""}"#;
    let at_limit = format!(
        "{prefix}{}{suffix}",
        "x".repeat(8192 - prefix.len() - suffix.len())
    );
    let oversized = format!("{at_limit} ");
    for document in [&at_limit, &oversized] {
        let results = [
            cli.parse_from(
                ["test", "lists", "document", "edit", "--json", document],
                Unread,
            ),
            cli.parse_from(
                ["test", "lists", "document", "edit", "--json", "-"],
                document.as_bytes(),
            ),
        ];
        for result in results {
            if document.len() == 8192 {
                assert_eq!(
                    result.unwrap().input["value"].as_str().unwrap().len(),
                    8192 - prefix.len() - suffix.len()
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.exit_code(), 2);
                assert!(error.to_string().contains("bounds.message_bytes"));
            }
        }
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn schema_refusals_are_carried_unchanged_for_flags_json_and_prepared_calls() {
    for (schema, flag, text, input) in [
        (
            json!({"type":"string","minLength":2}),
            "--value",
            "x",
            json!({"value":"x"}),
        ),
        (
            json!({"enum":["a","b"]}),
            "--value",
            "c",
            json!({"value":"c"}),
        ),
        (
            json!({"type":"integer","minimum":2}),
            "--value",
            "1",
            json!({"value":1}),
        ),
        (
            json!({"type":"array","items":{"type":"integer"}}),
            "--value-json",
            r#"["x"]"#,
            json!({"value":["x"]}),
        ),
    ] {
        let operation =
            descriptor(json!({"type":"object","properties":{"value":schema},"required":["value"]}));
        let set = clapham::compose(
            [Contribution::new("lists")
                .operation(operation.clone(), |_: Call| async { unreachable!() })],
            Bounds::default(),
        )
        .unwrap();
        let expected = set
            .get("lists.document.edit")
            .unwrap()
            .validate_input(&input)
            .unwrap_err();
        let (cli, calls) = fixture(operation, ExecutionMode::InProcess, Bounds::default());
        let document = input.to_string();
        let errors = [
            run(&cli, &["test", "lists", "document", "edit", flag, text]).unwrap_err(),
            cli.parse_from(
                ["test", "lists", "document", "edit", "--json", &document],
                Unread,
            )
            .unwrap_err(),
            cli.parse_from(
                ["test", "lists", "document", "edit", "--json", "-"],
                document.as_bytes(),
            )
            .unwrap_err(),
        ];
        for error in errors {
            assert_eq!(error.exit_code(), 65);
            assert_eq!(error, CliError::Refusal(expected.clone()));
        }
        // Public Invocation fields can be set without parse_from, so call must
        // apply the same schema validator before the handler is reached.
        let invocation = clapham_cli::Invocation {
            name: "lists.document.edit".into(),
            principal: Principal::new("account", PrincipalKind::Human),
            input,
        };
        assert_eq!(
            block_on(cli.call(invocation, RecordSink::default(), Cancellation::new())).unwrap_err(),
            expected
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn describe_namespace_conflicts_with_the_reserved_alias() {
    for name in ["describe.read", "describe.document.read"] {
        let mut operation = descriptor(json!({"type":"object"}));
        operation.name = name.parse().unwrap();
        let set = clapham::compose(
            [Contribution::new("describe")
                .operation(operation, |_: Call| async { unreachable!() })],
            Bounds::default(),
        )
        .unwrap();
        let error =
            Cli::new("test", set, Principal::new("account", PrincipalKind::Human)).unwrap_err();
        assert_eq!(error.code, "cli_configuration");
        assert!(error.message.contains("describe"));
    }
}

#[test]
fn required_positionals_can_be_followed_by_optional_ones() {
    let mut operation = descriptor(
        json!({"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},"required":["a"]}),
    );
    operation.positional = vec!["a".into(), "b".into()];
    let (cli, _) = fixture(operation, ExecutionMode::InProcess, Bounds::default());
    assert_eq!(
        cli.parse_from(["test", "lists", "document", "edit", "first"], Unread)
            .unwrap()
            .input,
        json!({"a":"first"})
    );
    assert_eq!(
        cli.parse_from(
            ["test", "lists", "document", "edit", "first", "second"],
            Unread
        )
        .unwrap()
        .input,
        json!({"a":"first","b":"second"})
    );
}

#[test]
fn a_type_next_to_ref_controls_the_argument_spelling_and_conversion() {
    let (cli, calls) = fixture(
        descriptor(json!({
            "type":"object", "$defs":{"Open":{}}, "properties":{
                "value":{"$ref":"#/$defs/Open","type":"integer","description":"The item count."}
            },"required":["value"]
        })),
        ExecutionMode::InProcess,
        Bounds::default(),
    );
    assert_eq!(
        cli.parse_from(
            ["test", "lists", "document", "edit", "--value", "3"],
            Unread
        )
        .unwrap()
        .input,
        json!({"value":3})
    );
    assert_eq!(
        cli.parse_from(
            ["test", "lists", "document", "edit", "--value", "false"],
            Unread
        )
        .unwrap_err()
        .exit_code(),
        2
    );
    assert!(
        cli.parse_from(["test", "lists", "document", "edit", "--help"], Unread)
            .unwrap_err()
            .to_string()
            .contains("The item count.")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
