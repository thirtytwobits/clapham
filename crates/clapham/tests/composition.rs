//! Composition, compiled validation, deterministic revisions and scripted dispatch.

use std::future::Future;
use std::pin::pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use clapham::{
    Bounds, Call, Category, ComposedSet, CompositionCode, Contribution, Evidence, Handler,
    HandlerFuture, Operation, Outcome, Principal, PrincipalKind, RecordSink, Refusal, compose,
};
use futures_executor::block_on;
use futures_util::task::noop_waker;
use serde_json::{Value, json};

fn descriptor(name: &str) -> Operation {
    serde_json::from_value(json!({
        "name": name, "description": "Read an item.",
        "input": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
        "output": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
        "effect": "query", "locality": "workspace", "refusals": { "list_missing": "not_found" },
        "idempotent": true, "open_world": false
    })).unwrap()
}

async fn echo(call: Call) -> Result<Outcome, Refusal> {
    Ok(Outcome::new(call.input))
}

fn one(operation: Operation, bounds: Bounds) -> Result<ComposedSet, clapham::CompositionError> {
    compose(
        [Contribution::new("lists").operation(operation, echo)],
        bounds,
    )
}

fn call() -> Call {
    Call::new(
        "front.alias".parse().unwrap(),
        Principal::new("reader", PrincipalKind::Human),
        json!({ "id": "item-1" }),
    )
}

#[test]
fn composes_two_namespaces_in_name_order_and_retains_their_own_names() {
    let set = compose(
        [
            Contribution::new("lists").operation(descriptor("lists.items.get"), echo),
            Contribution::new("items").operation(descriptor("items.get"), echo),
        ],
        Bounds::default(),
    )
    .unwrap();
    assert_eq!(
        set.operations()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        [
            "clapham.describe",
            "clapham.version",
            "items.get",
            "lists.items.get"
        ]
    );
    assert!(set.get("missing.get").is_none());
    assert_eq!(
        set.get("items.get").unwrap().descriptor().name.as_str(),
        "items.get"
    );
    assert_eq!(set.bounds(), &Bounds::default());
    let clone = set.clone();
    assert_eq!(clone.revision(), set.revision());
    assert!(std::ptr::eq(
        set.get("items.get").unwrap(),
        clone.get("items.get").unwrap()
    ));
}

#[test]
fn refuses_repeated_namespaces_even_without_operations_and_reserves_its_own() {
    for contributions in [
        vec![Contribution::new("lists"), Contribution::new("lists")],
        vec![Contribution::new("clapham")],
    ] {
        let error = compose(contributions, Bounds::default()).unwrap_err();
        assert_eq!(error.code.as_str(), "namespace_claimed");
    }
}

#[test]
fn refuses_names_outside_the_namespace_alphabet_and_operations_owned_elsewhere() {
    for namespace in [
        "",
        "lists items",
        "lists/items",
        "lists.é",
        "lists\n",
        "lists.items",
    ] {
        assert_eq!(
            compose([Contribution::new(namespace)], Bounds::default())
                .unwrap_err()
                .code
                .as_str(),
            "invalid_name"
        );
    }
    for name in ["items.get", "items", ".get", "lists."] {
        assert_eq!(
            one(descriptor(name), Bounds::default())
                .unwrap_err()
                .code
                .as_str(),
            "invalid_name"
        );
    }
    // Invalid complete-name characters are refused before an Operation can exist.
    assert!("lists.items/get".parse::<clapham::OperationName>().is_err());
}

#[test]
fn refuses_empty_and_whitespace_descriptions_without_rewriting_valid_text() {
    for text in ["", " \n\t"] {
        let mut operation = descriptor("lists.items.get");
        operation.description = text.into();
        assert_eq!(
            one(operation, Bounds::default()).unwrap_err().code.as_str(),
            "missing_description"
        );
    }
    let mut operation = descriptor("lists.items.get");
    operation.description = "  Read an item.  ".into();
    let set = one(operation, Bounds::default()).unwrap();
    assert_eq!(
        set.get("lists.items.get").unwrap().descriptor().description,
        "  Read an item.  "
    );
}

#[test]
fn refuses_invalid_input_and_output_schemas_and_unresolved_external_references() {
    for input in [
        json!({}),
        json!({ "type": "array" }),
        json!({ "type": "object", "properties": { "id": { "type": 17 } } }),
        json!({ "type": "object", "$ref": "#/$defs/missing" }),
        json!({ "type": "object", "$ref": "file:///missing/schema.json" }),
        json!({ "type": "object", "$ref": "https://example.invalid/schema.json" }),
    ] {
        let mut operation = descriptor("lists.items.get");
        operation.input = input.as_object().unwrap().clone();
        assert_eq!(
            one(operation, Bounds::default()).unwrap_err().code.as_str(),
            "invalid_schema"
        );
    }
    for output in [
        json!({ "type": 17 }),
        json!({ "$ref": "#/$defs/missing" }),
        json!({ "$ref": "https://example.invalid/schema.json" }),
    ] {
        let mut operation = descriptor("lists.items.get");
        operation.output = Some(output.try_into().unwrap());
        assert_eq!(
            one(operation, Bounds::default()).unwrap_err().code.as_str(),
            "invalid_schema"
        );
    }
    let mut operation = descriptor("lists.items.get");
    operation.input = json!({ "type": "object", "$defs": { "Id": { "type": "string" } }, "properties": { "id": { "$ref": "#/$defs/Id" } }, "required": ["id"] }).as_object().unwrap().clone();
    let set = one(operation, Bounds::default()).unwrap();
    assert!(
        set.get("lists.items.get")
            .unwrap()
            .validate_input(&json!({ "id": "item-1" }))
            .is_ok()
    );
    assert!(
        set.get("lists.items.get")
            .unwrap()
            .validate_input(&json!({ "id": 1 }))
            .is_err()
    );
}

#[test]
fn checks_each_schema_size_at_the_exact_serialised_byte_boundary() {
    for input in [true, false] {
        let mut operation = descriptor("lists.items.get");
        // Exceed both built-in schemas so this descriptor determines the limit.
        let schema = json!({ "type": "object", "description": "é".repeat(20_000) });
        if input {
            operation.input = schema.as_object().unwrap().clone();
        } else {
            operation.output = Some(schema.clone().try_into().unwrap());
        }
        let bytes = serde_json::to_vec(&schema).unwrap().len();
        assert!(
            one(
                operation.clone(),
                Bounds {
                    schema_bytes: bytes,
                    ..Bounds::default()
                }
            )
            .is_ok()
        );
        assert_eq!(
            one(
                operation,
                Bounds {
                    schema_bytes: bytes - 1,
                    ..Bounds::default()
                }
            )
            .unwrap_err()
            .code
            .as_str(),
            "schema_too_large"
        );
    }
}

#[test]
fn refuses_duplicate_operations_and_catalogue_count_or_byte_overflow() {
    let repeated = Contribution::new("lists")
        .operation(descriptor("lists.items.get"), echo)
        .operation(descriptor("lists.items.get"), echo);
    assert_eq!(
        compose([repeated], Bounds::default()).unwrap_err().code,
        CompositionCode::DuplicateOperation
    );
    let contribution = Contribution::new("lists")
        .operation(descriptor("lists.items.get"), echo)
        .operation(descriptor("lists.items.read"), echo);
    assert_eq!(
        compose(
            [contribution],
            Bounds {
                tools: 1,
                ..Bounds::default()
            }
        )
        .unwrap_err()
        .code,
        CompositionCode::TooManyOperations
    );
    let operation = descriptor("lists.items.get");
    let baseline = one(operation.clone(), Bounds::default()).unwrap();
    let bytes = serde_json::to_vec(&baseline.operations().collect::<Vec<_>>())
        .unwrap()
        .len();
    assert!(
        one(
            operation.clone(),
            Bounds {
                catalogue_bytes: bytes,
                ..Bounds::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        one(
            operation,
            Bounds {
                catalogue_bytes: bytes - 1,
                ..Bounds::default()
            }
        )
        .unwrap_err()
        .code,
        CompositionCode::CatalogueTooLarge
    );
}

struct BorrowingHandler(String);
impl Handler for BorrowingHandler {
    fn call(&self, call: Call) -> HandlerFuture<'_> {
        Box::pin(async move {
            assert_eq!(call.name.as_str(), "lists.items.get");
            assert_eq!(
                call.principal,
                Principal::new("reader", PrincipalKind::Human)
            );
            assert_eq!(call.input, json!({ "id": "item-1" }));
            assert!(call.preview);
            Ok(Outcome::new(json!({ "id": self.0 })))
        })
    }
}

#[test]
fn boxed_handlers_can_borrow_state_and_receive_validated_input_under_the_own_name() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>(_: &T) {}
    assert_send_sync::<ComposedSet>();
    let handler: Box<dyn Handler> = Box::new(BorrowingHandler("item-2".into()));
    let set = compose(
        [Contribution::new("lists").operation_boxed(descriptor("lists.items.get"), handler)],
        Bounds::default(),
    )
    .unwrap();
    let mut call = call();
    call.preview = true;
    let future = set.get("lists.items.get").unwrap().invoke(call);
    assert_send(&future);
    let outcome = block_on(future).unwrap();
    assert_eq!(outcome.result, Some(json!({ "id": "item-2" })));
    assert_eq!(outcome.evidence, Evidence::Completed);
}

#[test]
fn invalid_input_and_pre_cancelled_calls_never_enter_the_handler() {
    let entries = Arc::new(AtomicUsize::new(0));
    let observed = entries.clone();
    let set =
        compose(
            [Contribution::new("lists").operation(
                descriptor("lists.items.get"),
                move |call: Call| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    echo(call)
                },
            )],
            Bounds::default(),
        )
        .unwrap();
    let operation = set.get("lists.items.get").unwrap();
    let mut invalid = call();
    invalid.input = json!({ "id": 17 });
    let refusal = block_on(operation.invoke(invalid)).unwrap_err();
    assert_eq!(refusal.code, "invalid_input");
    assert_eq!(refusal.evidence, Evidence::NotDispatched);
    let cancelled = call();
    cancelled.cancel.cancel();
    let refusal = block_on(operation.invoke(cancelled)).unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::NotDispatched);
    assert_eq!(entries.load(Ordering::SeqCst), 0);
}

#[test]
fn validates_successful_output_but_retains_tool_reported_errors() {
    let set = one(descriptor("lists.items.get"), Bounds::default()).unwrap();
    let operation = set.get("lists.items.get").unwrap();
    for result in [None, Some(Value::Null), Some(json!({ "id": 17 }))] {
        let mut outcome = Outcome::new(Value::Null);
        outcome.result = result;
        let refusal = operation.validate_output(&outcome).unwrap_err();
        assert_eq!(refusal.code, "invalid_result");
        assert_eq!(refusal.evidence, Evidence::Completed);
        outcome.is_error = true;
        assert!(operation.validate_output(&outcome).is_ok());
    }
    let set = compose(
        [
            Contribution::new("lists").operation(descriptor("lists.items.get"), |_| async {
                Ok(Outcome::new(json!({ "id": 17 })))
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    assert_eq!(
        block_on(set.get("lists.items.get").unwrap().invoke(call()))
            .unwrap_err()
            .code,
        "invalid_result"
    );
}

#[test]
fn handler_refusals_and_uncertain_remote_evidence_pass_through_unchanged() {
    for evidence in [
        Evidence::NotDispatched,
        Evidence::Completed,
        Evidence::Uncertain,
    ] {
        let expected = Refusal::new("list_missing", Category::NotFound, "The list is absent.")
            .with_evidence(evidence);
        let returned = expected.clone();
        let set = compose(
            [
                Contribution::new("lists").operation(descriptor("lists.items.get"), move |_| {
                    let returned = returned.clone();
                    async move { Err(returned) }
                }),
            ],
            Bounds::default(),
        )
        .unwrap();
        assert_eq!(
            block_on(set.get("lists.items.get").unwrap().invoke(call())).unwrap_err(),
            expected
        );
    }
}

#[test]
fn records_arrive_before_the_result_and_the_sink_applies_backpressure() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let completed_send = Arc::new(AtomicBool::new(false));
    let observed = completed_send.clone();
    let (release, held) = futures_channel::oneshot::channel::<()>();
    let held = Arc::new(Mutex::new(Some(held)));
    let output = records.clone();
    let sink = RecordSink::new(move |record| {
        output.lock().unwrap().push(record);
        let held = held.lock().unwrap().take().unwrap();
        async move {
            held.await.unwrap();
            Ok(())
        }
    });
    let mut descriptor = descriptor("lists.items.get");
    descriptor.streams = true;
    let set = compose(
        [
            Contribution::new("lists").operation(descriptor, move |call: Call| {
                let observed = observed.clone();
                async move {
                    call.records.send(json!({ "id": "item-1" })).await?;
                    observed.store(true, Ordering::SeqCst);
                    Ok(Outcome::new(call.input))
                }
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    let mut call = call();
    call.records = sink;
    let mut invocation = pin!(set.get("lists.items.get").unwrap().invoke(call));
    let waker = noop_waker();
    assert!(
        invocation
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert_eq!(*records.lock().unwrap(), [json!({ "id": "item-1" })]);
    assert!(!completed_send.load(Ordering::SeqCst));
    release.send(()).unwrap();
    assert_eq!(block_on(invocation).unwrap().evidence, Evidence::Completed);
    assert!(completed_send.load(Ordering::SeqCst));
}

#[test]
fn cancelling_a_scripted_in_flight_handler_yields_uncertain_evidence() {
    struct Entered(Arc<AtomicBool>);
    impl Drop for Entered {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (release, held) = futures_channel::oneshot::channel::<()>();
    let held = Mutex::new(Some(held));
    let entered = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicBool::new(false));
    let observed = entered.clone();
    let exited = dropped.clone();
    let set =
        compose(
            [Contribution::new("lists").operation(
                descriptor("lists.items.get"),
                move |call: Call| {
                    let held = held.lock().unwrap().take().unwrap();
                    let observed = observed.clone();
                    let exited = exited.clone();
                    async move {
                        let _entered = Entered(exited);
                        observed.store(true, Ordering::SeqCst);
                        held.await.unwrap();
                        Ok(Outcome::new(call.input))
                    }
                },
            )],
            Bounds::default(),
        )
        .unwrap();
    let call = call();
    let cancel = call.cancel.clone();
    let mut invocation = pin!(set.get("lists.items.get").unwrap().invoke(call));
    let waker = noop_waker();
    assert!(
        invocation
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert!(entered.load(Ordering::SeqCst));
    cancel.cancel();
    let Poll::Ready(Err(refusal)) = invocation.as_mut().poll(&mut Context::from_waker(&waker))
    else {
        panic!("cancellation did not stop dispatch")
    };
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert!(refusal.retryable);
    assert!(dropped.load(Ordering::SeqCst));
    assert!(release.send(()).is_err());
}

#[test]
fn revision_is_independent_of_contribution_and_nested_object_key_order() {
    let first = descriptor("lists.items.get");
    let mut reordered = first.clone();
    reordered.input = serde_json::from_str::<Value>(r#"{"additionalProperties":false,"required":["id"],"properties":{"id":{"type":"string"}},"type":"object"}"#).unwrap().as_object().unwrap().clone();
    let make = |operation, reverse| {
        let mut contributions = vec![
            Contribution::new("lists").operation(operation, echo),
            Contribution::new("items").operation(descriptor("items.get"), echo),
        ];
        if reverse {
            contributions.reverse();
        }
        compose(contributions, Bounds::default()).unwrap()
    };
    assert_eq!(
        make(first, false).revision(),
        make(reordered, true).revision()
    );
}

#[test]
fn revision_excludes_handlers_bounds_and_operation_construction_order() {
    let first = compose(
        [Contribution::new("lists")
            .operation(descriptor("lists.items.get"), echo)
            .operation(descriptor("lists.items.read"), echo)],
        Bounds::default(),
    )
    .unwrap();
    let second = compose(
        [Contribution::new("lists")
            .operation(descriptor("lists.items.read"), |_| async {
                Ok(Outcome::new(Value::Null))
            })
            .operation(descriptor("lists.items.get"), |_| async {
                Ok(Outcome::new(Value::Null))
            })],
        Bounds {
            tools: 1024,
            ..Bounds::default()
        },
    )
    .unwrap();
    assert_eq!(first.revision(), second.revision());
}

#[test]
fn every_descriptor_field_participates_in_the_revision() {
    let original = descriptor("lists.items.get");
    let revision = one(original.clone(), Bounds::default())
        .unwrap()
        .revision()
        .to_owned();
    for (field, value) in [
        ("name", json!("lists.items.read")),
        ("title", json!("Read")),
        ("description", json!("Read another item.")),
        ("input", json!({ "type": "object" })),
        ("output", json!(true)),
        ("effect", json!("mutation")),
        ("principals", json!(["human"])),
        ("locality", json!("daemon")),
        ("refusals", json!({ "list_missing": "conflict" })),
        ("streams", json!(true)),
        ("resource", json!("lists://{id}")),
        ("preview", json!(true)),
        ("positional", json!(["id"])),
        ("idempotent", json!(false)),
        ("open_world", json!(true)),
    ] {
        let mut wire = serde_json::to_value(&original).unwrap();
        wire[field] = value;
        let changed = one(serde_json::from_value(wire).unwrap(), Bounds::default()).unwrap();
        assert_ne!(changed.revision(), revision, "revision omitted {field}");
    }
}

#[test]
#[ignore = "child-process revision probe; invoked by revision_is_stable_across_processes"]
fn revision_probe() {
    println!(
        "REVISION={}",
        one(descriptor("lists.items.get"), Bounds::default())
            .unwrap()
            .revision()
    );
}

#[test]
fn revision_v1_matches_the_independent_golden_digest() {
    // The fixture has deliberately unordered nested keys and omitted principal
    // defaults. This literal pins the documented preimage across both map backends.
    let operation: Operation =
        serde_json::from_str(include_str!("fixtures/revision-v1.json")).unwrap();
    assert_eq!(
        one(operation, Bounds::default())
            .unwrap()
            .select(["lists.items.get"])
            .unwrap()
            .revision(),
        "sha256:0cf27f0b33f394b0fa9d5048705443e9150834eaa75c602a1d651449b10f86e8"
    );
}

#[test]
fn an_empty_principal_list_composes_without_granting_default_kinds() {
    let mut operation = descriptor("lists.items.get");
    operation.principals.clear();
    let set = one(operation, Bounds::default()).unwrap();
    assert!(
        set.get("lists.items.get")
            .unwrap()
            .descriptor()
            .principals
            .is_empty()
    );
}

#[test]
fn output_schemas_can_describe_any_json_result_shape() {
    for (schema, result) in [
        (
            json!({ "type": "array", "items": { "type": "integer" } }),
            json!([1, 2]),
        ),
        (json!({ "type": "string" }), json!("item-1")),
        (json!({ "type": "number" }), json!(1.5)),
        (json!({ "type": "boolean" }), json!(true)),
        (json!({ "type": "null" }), Value::Null),
        (json!(true), json!([1, "item-1"])),
    ] {
        let mut descriptor = descriptor("lists.items.get");
        descriptor.output = Some(schema.try_into().unwrap());
        let expected = result.clone();
        let set = compose(
            [Contribution::new("lists").operation(descriptor, move |_| {
                let result = result.clone();
                async move { Ok(Outcome::new(result)) }
            })],
            Bounds::default(),
        )
        .unwrap();
        let outcome = block_on(set.get("lists.items.get").unwrap().invoke(call())).unwrap();
        assert_eq!(outcome.result, Some(expected));
    }
}

#[test]
fn a_sink_failure_with_unresolved_effects_keeps_uncertain_evidence() {
    let refusal = Refusal::new(
        "record_delivery_failed",
        Category::Unavailable,
        "The record was not acknowledged.",
    )
    .with_evidence(Evidence::Uncertain);
    let expected = refusal.clone();
    let mut descriptor = descriptor("lists.items.get");
    descriptor.streams = true;
    let set = compose(
        [
            Contribution::new("lists").operation(descriptor, |call: Call| async move {
                call.records.send(json!({ "id": "item-1" })).await?;
                Ok(Outcome::new(call.input))
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    let mut call = call();
    call.records = RecordSink::new(move |_| {
        let refusal = refusal.clone();
        async move { Err(refusal) }
    });
    assert_eq!(
        block_on(set.get("lists.items.get").unwrap().invoke(call)).unwrap_err(),
        expected
    );
}

#[test]
fn cancellation_wins_when_the_held_handler_is_also_ready_to_complete() {
    let (release, held) = futures_channel::oneshot::channel::<()>();
    let held = Mutex::new(Some(held));
    let set =
        compose(
            [Contribution::new("lists").operation(
                descriptor("lists.items.get"),
                move |call: Call| {
                    let held = held.lock().unwrap().take().unwrap();
                    async move {
                        held.await.unwrap();
                        Ok(Outcome::new(call.input))
                    }
                },
            )],
            Bounds::default(),
        )
        .unwrap();
    let call = call();
    let cancel = call.cancel.clone();
    let mut invocation = pin!(set.get("lists.items.get").unwrap().invoke(call));
    let waker = noop_waker();
    assert!(
        invocation
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    // Both awaited futures become ready before the next poll.
    release.send(()).unwrap();
    cancel.cancel();
    let refusal = block_on(invocation).unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::Uncertain);
}

#[test]
fn revision_is_stable_across_processes() {
    let expected = one(descriptor("lists.items.get"), Bounds::default()).unwrap();
    for _ in 0..2 {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "revision_probe", "--nocapture"])
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let stdout = String::from_utf8(child.stdout).unwrap();
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("REVISION={}", expected.revision())),
            "{stdout}"
        );
    }
}
