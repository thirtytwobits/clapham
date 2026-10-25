//! Selection authority, admission ordering and scripted calls through the composed set.

use std::future::Future;
use std::pin::pin;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use clapham::{
    Admission, Bounds, Call, CallResult, Cancellation, Category, ComposeOptions, ComposedSet,
    Contribution, Effect, Evidence, ExecutionMode, Handler, OAuthContext, Operation, Outcome,
    RecordSink, Refusal, Selection, compose, compose_with_options,
};
use futures_executor::block_on;
use futures_util::task::noop_waker;
use serde_json::{Value, json};

fn descriptor(name: &str, effect: Effect) -> Operation {
    serde_json::from_value(json!({
        "name": name, "description": "Read or change a list item.",
        "input": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
        "output": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false },
        "effect": effect, "locality": "workspace", "refusals": { "list_missing": "not_found" },
        "idempotent": true, "open_world": false
    })).unwrap()
}

fn auth(kind: &[&str]) -> OAuthContext {
    oauth_context("list-reader", kind)
}
fn input() -> Value {
    json!({ "id": "item-1" })
}
async fn echo(call: Call) -> CallResult {
    Ok(Outcome::new(call.input))
}

fn one(operation: Operation, handler: impl Handler + 'static) -> ComposedSet {
    compose(
        [Contribution::new("lists").operation(operation, handler)],
        Bounds::default(),
    )
    .unwrap()
}

fn run(set: &ComposedSet, selection: &Selection, name: &str) -> CallResult {
    block_on(set.call(
        selection,
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        name,
        input(),
        RecordSink::default(),
        Cancellation::new(),
        false,
    ))
}

#[test]
fn selection_lists_a_sorted_deduplicated_subset_and_refuses_unknown_names() {
    let set = compose(
        [Contribution::new("lists")
            .operation(descriptor("lists.get", Effect::Query), echo)
            .operation(descriptor("lists.read", Effect::Query), echo)],
        Bounds::default(),
    )
    .unwrap();
    let selection = set
        .select(["lists.read", "lists.get", "lists.read"])
        .unwrap();
    assert_eq!(
        selection
            .operations()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        ["lists.get", "lists.read"]
    );
    assert!(selection.contains("lists.get"));
    assert!(!selection.contains("lists.absent"));
    let empty = set.select([] as [&str; 0]).unwrap();
    assert_eq!(empty.operations().len(), 0);
    let refusal = set.select(["lists.get", "lists.absent"]).unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category, refusal.evidence),
        (
            "unknown_operation",
            Category::NotFound,
            Evidence::NotDispatched
        )
    );
}

#[test]
fn an_unselected_or_foreign_selection_never_runs_a_handler_or_gate() {
    let entries = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::new(AtomicUsize::new(0));
    let observed = entries.clone();
    let gate = admitted.clone();
    let set = compose_with_options(
        [Contribution::new("lists").operation(
            descriptor("lists.get", Effect::Query),
            move |call: Call| {
                observed.fetch_add(1, Ordering::SeqCst);
                echo(call)
            },
        )],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                gate.fetch_add(1, Ordering::SeqCst);
                Admission::Allow
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let foreign = one(descriptor("lists.get", Effect::Query), echo);
    // Equal descriptors/revisions do not confer authority over another set.
    assert_eq!(set.all().revision(), foreign.all().revision());
    for selection in [set.select([] as [&str; 0]).unwrap(), foreign.all()] {
        let refusal = run(&set, &selection, "lists.get").unwrap_err();
        assert_eq!(refusal.code, "unknown_operation");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
    }
    assert_eq!(entries.load(Ordering::SeqCst), 0);
    assert_eq!(admitted.load(Ordering::SeqCst), 0);
    let clone = set.clone();
    assert_eq!(
        run(&clone, &set.all(), "lists.get").unwrap().result,
        Some(input())
    );
}

#[test]
fn input_validation_precedes_the_gate_and_prevents_dispatch() {
    let admitted = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let seen = admitted.clone();
    let called = entered.clone();
    let set = compose_with_options(
        [Contribution::new("lists").operation(
            descriptor("lists.get", Effect::Query),
            move |call: Call| {
                called.fetch_add(1, Ordering::SeqCst);
                echo(call)
            },
        )],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                seen.fetch_add(1, Ordering::SeqCst);
                Admission::Deny
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let refusal = block_on(set.call(
        &set.all(),
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        "lists.get",
        json!({ "id": 17 }),
        RecordSink::default(),
        Cancellation::new(),
        true,
    ))
    .unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category, refusal.evidence),
        (
            "invalid_input",
            Category::InvalidInput,
            Evidence::NotDispatched
        )
    );
    assert_eq!(admitted.load(Ordering::SeqCst), 0);
    assert_eq!(entered.load(Ordering::SeqCst), 0);
}

#[test]
fn a_host_gate_can_admit_an_agent_to_an_administrative_operation() {
    let set = compose_with_options(
        [Contribution::new("lists").operation(
            descriptor("lists.compact", Effect::Administrative),
            |call: Call| async move {
                assert_eq!(call.auth, auth(&["lists:read", "lists:write"][..]));
                assert_eq!(call.name.as_str(), "lists.compact");
                Ok(Outcome::new(call.input))
            },
        )],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(|_: &OAuthContext, _: &Operation| Admission::Allow),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let result = block_on(set.call(
        &set.all(),
        auth(&["lists:read", "lists:write"][..]),
        "lists.compact",
        input(),
        RecordSink::default(),
        Cancellation::new(),
        false,
    ))
    .unwrap();
    assert_eq!(result.result, Some(input()));
}

#[test]
fn pending_and_denied_admission_do_not_dispatch_and_precede_preview_checks() {
    for decision in [Admission::Deny, Admission::Pending] {
        let entered = Arc::new(AtomicBool::new(false));
        let observed = entered.clone();
        let set = compose_with_options(
            [Contribution::new("lists").operation(
                descriptor("lists.get", Effect::Query),
                move |call: Call| {
                    observed.store(true, Ordering::SeqCst);
                    echo(call)
                },
            )],
            Bounds::default(),
            ComposeOptions {
                gate: Arc::new(move |_: &OAuthContext, _: &Operation| decision),
                ..ComposeOptions::default()
            },
        )
        .unwrap();
        let refusal = block_on(set.call(
            &set.all(),
            auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
            "lists.get",
            input(),
            RecordSink::default(),
            Cancellation::new(),
            true,
        ))
        .unwrap_err();
        assert_eq!(refusal.category, Category::Refused);
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
        assert_eq!(refusal.retryable, decision == Admission::Pending);
        assert_eq!(
            refusal.code,
            if decision == Admission::Pending {
                "admission_pending"
            } else {
                "not_admitted"
            }
        );
        assert!(!entered.load(Ordering::SeqCst));
    }
}

#[test]
fn gate_decisions_are_consulted_on_every_call() {
    let allow = Arc::new(AtomicBool::new(false));
    let observed = allow.clone();
    let set = compose_with_options(
        [Contribution::new("lists").operation(descriptor("lists.get", Effect::Query), echo)],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                if observed.load(Ordering::SeqCst) {
                    Admission::Allow
                } else {
                    Admission::Deny
                }
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let selection = set.all();
    assert_eq!(
        run(&set, &selection, "lists.get").unwrap_err().code,
        "not_admitted"
    );
    allow.store(true, Ordering::SeqCst);
    assert!(run(&set, &selection, "lists.get").is_ok());
}

#[test]
fn preview_requires_support_and_is_passed_unchanged_to_the_handler() {
    for supported in [false, true] {
        let entered = Arc::new(AtomicBool::new(false));
        let observed = entered.clone();
        let mut operation = descriptor("lists.create", Effect::Mutation);
        operation.preview = supported;
        let set = one(operation, move |call: Call| {
            observed.store(true, Ordering::SeqCst);
            assert!(call.preview);
            echo(call)
        });
        let result = block_on(set.call(
            &set.all(),
            auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
            "lists.create",
            input(),
            RecordSink::default(),
            Cancellation::new(),
            true,
        ));
        if supported {
            assert!(result.is_ok());
        } else {
            let refusal = result.unwrap_err();
            assert_eq!(
                (refusal.code.as_str(), refusal.category, refusal.evidence),
                (
                    "preview_unsupported",
                    Category::Usage,
                    Evidence::NotDispatched
                )
            );
        }
        assert_eq!(entered.load(Ordering::SeqCst), supported);
    }
}

#[test]
fn daemon_locality_refuses_in_process_but_runs_on_a_daemon() {
    for execution in [ExecutionMode::InProcess, ExecutionMode::Daemon] {
        let mut operation = descriptor("lists.get", Effect::Query);
        operation.locality = clapham::Locality::Daemon;
        let entered = Arc::new(AtomicBool::new(false));
        let observed = entered.clone();
        let set = compose_with_options(
            [
                Contribution::new("lists").operation(operation, move |call: Call| {
                    observed.store(true, Ordering::SeqCst);
                    echo(call)
                }),
            ],
            Bounds::default(),
            ComposeOptions {
                execution,
                ..ComposeOptions::default()
            },
        )
        .unwrap();
        let result = run(&set, &set.all(), "lists.get");
        if execution == ExecutionMode::InProcess {
            let refusal = result.unwrap_err();
            assert_eq!(
                (refusal.code.as_str(), refusal.category, refusal.evidence),
                ("no_daemon", Category::Unavailable, Evidence::NotDispatched)
            );
            assert!(!entered.load(Ordering::SeqCst));
        } else {
            assert_eq!(result.unwrap().result, Some(input()));
        }
    }
}

#[test]
fn invalid_output_is_an_internal_completed_refusal() {
    let set = one(descriptor("lists.get", Effect::Query), |_| async {
        Ok(Outcome::new(json!({ "id": 17 })))
    });
    let refusal = run(&set, &set.all(), "lists.get").unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category, refusal.evidence),
        ("invalid_result", Category::Internal, Evidence::Completed)
    );
}

#[test]
fn declared_refusals_keep_the_code_and_use_the_descriptors_category() {
    let mut original = Refusal::new("list_missing", Category::Internal, "List not found.")
        .with_evidence(Evidence::Uncertain);
    original.details = Some(json!({ "id": "list-1" }));
    original.retryable = true;
    let expected = original.clone();
    let set = one(descriptor("lists.get", Effect::Query), move |_| {
        let original = original.clone();
        async move { Err(original) }
    });
    let refusal = run(&set, &set.all(), "lists.get").unwrap_err();
    assert_eq!(refusal.category, Category::NotFound);
    assert_eq!(refusal.code, expected.code);
    assert_eq!(refusal.details, expected.details);
    assert_eq!(refusal.message, expected.message);
    assert_eq!(refusal.evidence, expected.evidence);
    assert!(refusal.retryable);
}

#[test]
fn undeclared_handler_refusals_preserve_the_original_including_reserved_looking_codes() {
    for code in [
        "list_fault",
        "cancelled",
        "invalid_input",
        "invalid_result",
        "no_daemon",
    ] {
        let mut original = Refusal::new(code, Category::Conflict, "The list changed.")
            .with_evidence(Evidence::Uncertain);
        original.retryable = true;
        let expected = serde_json::to_value(&original).unwrap();
        let set = one(descriptor("lists.get", Effect::Query), move |_| {
            let original = original.clone();
            async move { Err(original) }
        });
        let refusal = run(&set, &set.all(), "lists.get").unwrap_err();
        assert_eq!(
            (refusal.code.as_str(), refusal.category, refusal.evidence),
            (
                "undeclared_refusal",
                Category::Internal,
                Evidence::Uncertain
            )
        );
        assert_eq!(refusal.details, Some(expected));
        assert!(!refusal.retryable);
    }
}

fn refusing_sink() -> (RecordSink, Refusal) {
    let refusal = Refusal::new("output_failed", Category::Unavailable, "The output closed.")
        .with_evidence(Evidence::Uncertain);
    let returned = refusal.clone();
    let sink = RecordSink::new(move |_| {
        let refusal = returned.clone();
        async move { Err(refusal) }
    });
    (sink, refusal)
}

fn streaming(handler: impl Handler + 'static) -> ComposedSet {
    let mut operation = descriptor("lists.watch", Effect::Query);
    operation.streams = true;
    one(operation, handler)
}

fn watch(set: &ComposedSet, sink: RecordSink) -> CallResult {
    block_on(set.call(
        &set.all(),
        auth(&["lists:read"][..]),
        "lists.watch",
        input(),
        sink,
        Cancellation::new(),
        false,
    ))
}

#[test]
fn a_returned_sink_refusal_is_carried_unchanged_without_declaration() {
    let (sink, expected) = refusing_sink();
    let set = streaming(|call: Call| async move {
        call.records.send(json!({ "item": 1 })).await?;
        Ok(Outcome::new(call.input))
    });
    assert_eq!(watch(&set, sink).unwrap_err(), expected);
}

#[test]
fn an_earlier_sink_refusal_is_carried_unchanged_after_later_ones() {
    let failures = Arc::new(AtomicUsize::new(0));
    let sink = RecordSink::new(move |_| {
        let failure = failures.fetch_add(1, Ordering::SeqCst);
        async move {
            Err(Refusal::new(
                "output_failed",
                Category::Unavailable,
                format!("Failure {failure}."),
            )
            .with_evidence(Evidence::Uncertain))
        }
    });
    let set = streaming(|call: Call| async move {
        let first = call.records.send(json!({ "item": 1 })).await.unwrap_err();
        let later = call.records.send(json!({ "item": 2 })).await.unwrap_err();
        assert_ne!(first, later);
        Err(first)
    });
    let refusal = watch(&set, sink).unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("output_failed", "Failure 0.")
    );
}

#[test]
fn a_handler_refusal_other_than_the_sinks_still_requires_declaration() {
    for reuses_code in [true, false] {
        let (sink, original) = refusing_sink();
        let set = streaming(move |call: Call| async move {
            let refused = call.records.send(json!({ "item": 1 })).await.unwrap_err();
            let code = if reuses_code {
                refused.code
            } else {
                "list_fault".into()
            };
            // The handler's own refusal, even one reusing the sink's code.
            Err(Refusal::new(code, Category::Conflict, "The list changed."))
        });
        let refusal = watch(&set, sink).unwrap_err();
        assert_eq!(refusal.code, "undeclared_refusal", "{refusal:?}");
        assert_ne!(
            refusal.details,
            Some(serde_json::to_value(&original).unwrap())
        );
    }
}

#[test]
fn tool_reported_errors_remain_outcomes_with_their_own_result_shape() {
    let set = one(descriptor("lists.get", Effect::Query), |_| async {
        let mut outcome = Outcome::new(json!({ "code": "list_missing" }));
        outcome.is_error = true;
        Ok(outcome)
    });
    let outcome = run(&set, &set.all(), "lists.get").unwrap();
    assert!(outcome.is_error);
    assert_eq!(outcome.result.unwrap()["code"], "list_missing");
}

#[test]
fn pre_cancelled_calls_skip_validation_and_admission_without_dispatch() {
    for decision in [Admission::Allow, Admission::Deny, Admission::Pending] {
        let entered = Arc::new(AtomicBool::new(false));
        let observed = entered.clone();
        let consulted = Arc::new(AtomicUsize::new(0));
        let decisions = consulted.clone();
        let set = compose_with_options(
            [Contribution::new("lists").operation(
                descriptor("lists.get", Effect::Query),
                move |call: Call| {
                    observed.store(true, Ordering::SeqCst);
                    echo(call)
                },
            )],
            Bounds::default(),
            ComposeOptions {
                gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                    decisions.fetch_add(1, Ordering::SeqCst);
                    decision
                }),
                ..ComposeOptions::default()
            },
        )
        .unwrap();
        for (name, arguments) in [
            ("lists.get", input()),
            ("lists.get", json!({ "id": 17 })),
            ("lists.absent", input()),
        ] {
            let cancel = Cancellation::new();
            cancel.cancel();
            let refusal = block_on(set.call(
                &set.all(),
                auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
                name,
                arguments,
                RecordSink::default(),
                cancel,
                true,
            ))
            .unwrap_err();
            assert_eq!(
                (refusal.code.as_str(), refusal.category, refusal.evidence),
                ("cancelled", Category::Interrupted, Evidence::NotDispatched)
            );
            assert!(refusal.retryable);
        }
        assert_eq!(consulted.load(Ordering::SeqCst), 0);
        assert!(!entered.load(Ordering::SeqCst));
    }
}

#[test]
fn streaming_records_arrive_in_order_before_the_result_with_backpressure() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let (release, held) = futures_channel::oneshot::channel::<()>();
    let held = Mutex::new(Some(held));
    let output = records.clone();
    let sink = RecordSink::new(move |record| {
        output.lock().unwrap().push(record);
        let held = held.lock().unwrap().take();
        async move {
            if let Some(held) = held {
                held.await.unwrap();
            }
            Ok(())
        }
    });
    let mut operation = descriptor("lists.watch", Effect::Query);
    operation.streams = true;
    let set = one(operation, |call: Call| async move {
        call.records.send(json!({ "item": 1 })).await?;
        call.records.send(json!({ "item": 2 })).await?;
        Ok(Outcome::new(call.input))
    });
    let selection = set.all();
    let mut invocation = pin!(set.call(
        &selection,
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        "lists.watch",
        input(),
        sink,
        Cancellation::new(),
        false
    ));
    let waker = noop_waker();
    assert!(
        invocation
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert_eq!(*records.lock().unwrap(), [json!({ "item": 1 })]);
    release.send(()).unwrap();
    assert_eq!(block_on(invocation).unwrap().result, Some(input()));
    assert_eq!(
        *records.lock().unwrap(),
        [json!({ "item": 1 }), json!({ "item": 2 })]
    );
}

#[test]
fn cancellation_mid_stream_is_uncertain_without_becoming_an_undeclared_refusal() {
    let records = Arc::new(Mutex::new(Vec::new()));
    let (release, held) = futures_channel::oneshot::channel::<()>();
    let held = Mutex::new(Some(held));
    let output = records.clone();
    let sink = RecordSink::new(move |record| {
        output.lock().unwrap().push(record);
        let held = held.lock().unwrap().take().unwrap();
        async move {
            held.await.unwrap();
            Ok(())
        }
    });
    let mut operation = descriptor("lists.watch", Effect::Query);
    operation.streams = true;
    let set = one(operation, |call: Call| async move {
        call.records.send(json!({ "item": 1 })).await?;
        Ok(Outcome::new(call.input))
    });
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let selection = set.all();
    let mut invocation = pin!(set.call(
        &selection,
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        "lists.watch",
        input(),
        sink,
        cancel,
        false
    ));
    let waker = noop_waker();
    assert!(
        invocation
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    assert_eq!(records.lock().unwrap().len(), 1);
    signal.cancel();
    let Poll::Ready(Err(refusal)) = invocation.as_mut().poll(&mut Context::from_waker(&waker))
    else {
        panic!("stream did not cancel")
    };
    assert_eq!(
        (refusal.code.as_str(), refusal.category, refusal.evidence),
        ("cancelled", Category::Interrupted, Evidence::Uncertain)
    );
    assert!(refusal.retryable);
    assert!(release.send(()).is_err());
}

fn binding_set(revision: &str) -> ComposedSet {
    compose(
        [
            Contribution::new("lists").operation(descriptor("lists.get", Effect::Query), echo),
            Contribution::new("items")
                .with_binding_revision(revision)
                .operation(descriptor("items.get", Effect::Query), echo),
        ],
        Bounds::default(),
    )
    .unwrap()
}

#[test]
fn revision_selection_accounts_only_for_selected_descriptors_and_bindings() {
    let first = binding_set("binding-1");
    let changed = binding_set("binding-2");
    assert_eq!(first.revision(), changed.revision());
    assert_ne!(first.all().revision(), changed.all().revision());
    assert_ne!(
        first.select(["items.get"]).unwrap().revision(),
        changed.select(["items.get"]).unwrap().revision()
    );
    assert_eq!(
        first.select(["lists.get"]).unwrap().revision(),
        changed.select(["lists.get"]).unwrap().revision()
    );
    assert_ne!(
        first.all().revision(),
        first.select(["lists.get"]).unwrap().revision()
    );
    assert_eq!(
        first
            .select(["lists.get", "items.get", "lists.get"])
            .unwrap()
            .revision(),
        first.select(["items.get", "lists.get"]).unwrap().revision()
    );
}

#[test]
fn revision_unbound_selection_retains_the_composed_set_v1_framing() {
    let operation: Operation =
        serde_json::from_str(include_str!("fixtures/revision-v1.json")).unwrap();
    let set = one(operation, echo);
    assert_eq!(set.all().revision(), set.revision());
    assert_eq!(
        set.select(["lists.items.get"]).unwrap().revision(),
        "sha256:9adcd6f47686e790557eb091234c45b6801fe6fb93310d89579f456fbe1810cb"
    );
}

#[test]
fn revision_bound_selection_matches_an_independent_golden_digest() {
    let operation: Operation =
        serde_json::from_str(include_str!("fixtures/revision-v1.json")).unwrap();
    let set = compose(
        [Contribution::new("lists")
            .with_binding_revision("remote-v1")
            .operation(operation, echo)],
        Bounds::default(),
    )
    .unwrap();
    assert_eq!(
        set.select(["lists.items.get"]).unwrap().revision(),
        "sha256:1add19cb17d4cd52ea6b3a8acaca5d2f6601a149bcd5849d9990f4fbf4f63e5e"
    );
    assert_ne!(
        set.all().revision(),
        set.select(["lists.items.get"]).unwrap().revision()
    );
}

#[test]
#[ignore = "child-process revision probe; invoked by revision_binding_is_stable_across_processes"]
fn revision_binding_probe() {
    println!("REVISION={}", binding_set("binding-1").all().revision());
}

#[test]
fn revision_binding_is_stable_across_processes() {
    let expected = binding_set("binding-1").all().revision().to_owned();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "revision_binding_probe",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(
        String::from_utf8(child.stdout)
            .unwrap()
            .lines()
            .any(|line| line == format!("REVISION={expected}"))
    );
}

#[test]
fn a_gate_can_cancel_before_handler_entry_with_not_dispatched_evidence() {
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let entered = Arc::new(AtomicBool::new(false));
    let observed = entered.clone();
    let set = compose_with_options(
        [Contribution::new("lists").operation(
            descriptor("lists.get", Effect::Query),
            move |call: Call| {
                observed.store(true, Ordering::SeqCst);
                echo(call)
            },
        )],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &OAuthContext, _: &Operation| {
                signal.cancel();
                Admission::Allow
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let refusal = block_on(set.call(
        &set.all(),
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        "lists.get",
        input(),
        RecordSink::default(),
        cancel,
        false,
    ))
    .unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, Evidence::NotDispatched);
    assert!(!entered.load(Ordering::SeqCst));
}

#[test]
fn call_futures_and_selection_are_sendable_with_shared_immutable_handlers() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>(_: &T) {}
    assert_send_sync::<Selection>();
    let set = one(descriptor("lists.get", Effect::Query), echo);
    let selection = set.all();
    let future = set.call(
        &selection,
        auth(&["lists:read", "lists:write", "lists:admin", "items:tick"][..]),
        "lists.get",
        input(),
        RecordSink::default(),
        Cancellation::new(),
        false,
    );
    assert_send(&future);
    assert!(block_on(future).is_ok());
}

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}
