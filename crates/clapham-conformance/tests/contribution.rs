//! Deterministic lists-and-items calls, fenced writes, streams and scripting controls.

use std::future::Future;
use std::pin::pin;
use std::sync::{Arc, Mutex};
use std::task::Context;

use clapham::{
    Admission, Bounds, CallResult, Cancellation, Category, ComposeOptions, ComposedSet, Effect,
    Evidence, Gate, Principal, PrincipalKind, RecordSink, Refusal, compose_with_options,
};
use clapham_conformance::{ChangeKind, List, ListChange, ListsGate, ListsHandle};
use futures_executor::block_on;
use futures_util::{StreamExt, future::join, task::noop_waker};
use serde_json::{Value, json};

fn fixture() -> (ListsHandle, ComposedSet) {
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
    (handle, set)
}

fn invoke(
    set: &ComposedSet,
    name: &str,
    input: Value,
    kind: PrincipalKind,
    preview: bool,
) -> CallResult {
    block_on(set.call(
        &set.all(),
        Principal::new("list-reader", kind),
        name,
        input,
        RecordSink::default(),
        Cancellation::new(),
        preview,
    ))
}

fn call(set: &ComposedSet, name: &str, input: Value) -> Value {
    invoke(set, name, input, PrincipalKind::Human, false)
        .unwrap()
        .result
        .unwrap()
}

fn list(value: Value) -> List {
    serde_json::from_value(value).unwrap()
}

#[test]
fn all_rows_compose_with_the_effects_resources_positional_and_preview_metadata() {
    let (_, set) = fixture();
    assert_eq!(
        set.operations()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        [
            "clapham.describe",
            "clapham.version",
            "items.add",
            "items.tick",
            "lists.compact",
            "lists.create",
            "lists.delete",
            "lists.export",
            "lists.fault",
            "lists.get",
            "lists.list",
            "lists.rename",
            "lists.watch"
        ]
    );
    for name in ["lists.list", "lists.get", "lists.watch", "lists.export"] {
        assert_eq!(set.get(name).unwrap().descriptor().effect, Effect::Query);
    }
    assert_eq!(
        set.get("lists.compact").unwrap().descriptor().effect,
        Effect::Administrative
    );
    assert_eq!(
        set.get("lists.delete").unwrap().descriptor().effect,
        Effect::Destructive
    );
    for name in [
        "lists.create",
        "lists.rename",
        "lists.fault",
        "items.add",
        "items.tick",
    ] {
        assert_eq!(set.get(name).unwrap().descriptor().effect, Effect::Mutation);
    }
    assert_eq!(
        set.get("lists.list")
            .unwrap()
            .descriptor()
            .resource
            .as_deref(),
        Some("lists://")
    );
    let get = set.get("lists.get").unwrap().descriptor();
    assert_eq!(get.resource.as_deref(), Some("lists://{id}"));
    assert_eq!(get.positional, ["id"]);
    assert!(set.get("lists.create").unwrap().descriptor().preview);
    for name in ["lists.watch", "lists.export"] {
        assert!(set.get(name).unwrap().descriptor().streams);
    }
    for name in ["lists.compact", "lists.delete"] {
        let descriptor = set.get(name).unwrap().descriptor();
        assert_eq!(
            descriptor.principals,
            [
                PrincipalKind::Human,
                PrincipalKind::Service,
                PrincipalKind::System
            ]
        );
        for kind in [PrincipalKind::Service, PrincipalKind::System] {
            assert_eq!(
                ListsGate.admit(&Principal::new("list-reader", kind), descriptor),
                Admission::Allow
            );
        }
    }
}

#[test]
fn list_and_get_return_detached_snapshots_and_a_declared_missing_refusal() {
    let (handle, set) = fixture();
    assert_eq!(call(&set, "lists.list", json!({})), json!([]));
    handle.create_list("b", "Second").unwrap();
    handle.create_list("a", "First").unwrap();
    let snapshot = call(&set, "lists.list", json!({}));
    assert_eq!(snapshot, serde_json::to_value(handle.snapshot()).unwrap());
    assert_eq!(snapshot[0]["id"], "a");
    assert_eq!(
        call(&set, "lists.get", json!({"id":"b"})),
        serde_json::to_value(handle.get("b").unwrap()).unwrap()
    );
    handle.rename_list("b", "Changed").unwrap();
    assert_eq!(snapshot[1]["name"], "Second");
    let refusal = invoke(
        &set,
        "lists.get",
        json!({"id":"absent"}),
        PrincipalKind::Human,
        false,
    )
    .unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category),
        ("list_not_found", Category::NotFound)
    );
}

#[test]
fn object_creation_preview_has_the_real_shape_without_committing_or_advancing_revision() {
    let (handle, set) = fixture();
    let input = json!({"list":{"id":"a","name":"First"}});
    let preview = invoke(
        &set,
        "lists.create",
        input.clone(),
        PrincipalKind::Human,
        true,
    )
    .unwrap();
    assert_eq!(preview.evidence, Evidence::Completed);
    assert!(handle.get("a").is_none());
    assert!(handle.snapshot().is_empty());
    let created = call(&set, "lists.create", input.clone());
    assert_eq!(preview.result, Some(created.clone()));
    assert_eq!(list(created).revision, 1);
    let before = handle.snapshot();
    let refusal = invoke(&set, "lists.create", input, PrincipalKind::Human, false).unwrap_err();
    assert_eq!(refusal.code, "list_exists");
    assert_eq!(handle.snapshot(), before);
}

#[test]
fn a_held_fenced_rename_refuses_a_concurrent_change_and_preserves_the_new_state() {
    let (handle, set) = fixture();
    let original = handle.create_list("a", "Original").unwrap();
    let mut held = handle.hold("lists.rename").unwrap();
    let selection = set.all();
    let mut future = pin!(set.call(
        &selection,
        Principal::new("list-reader", PrincipalKind::Human),
        "lists.rename",
        json!({"id":"a","name":"Stale","revision":original.revision}),
        RecordSink::default(),
        Cancellation::new(),
        false
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert!(block_on(held.entered()));
    let current = handle.rename_list("a", "Concurrent").unwrap();
    assert!(held.release());
    let refusal = block_on(future).unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category),
        ("list_changed", Category::Conflict)
    );
    assert_eq!(refusal.details, Some(json!({"revision":current.revision})));
    assert_eq!(handle.get("a"), Some(current.clone()));
    let renamed = list(call(
        &set,
        "lists.rename",
        json!({"id":"a","name":"Fresh","revision":current.revision}),
    ));
    assert!(renamed.revision > current.revision);
    assert_eq!(renamed.name, "Fresh");
    // Integral JSON numbers have the same fence semantics as integer literals.
    let renamed_again = list(call(
        &set,
        "lists.rename",
        json!({"id":"a","name":"Final","revision":renamed.revision as f64}),
    ));
    assert_eq!(renamed_again.name, "Final");
}

#[test]
fn item_addition_and_tick_exercise_object_input_and_an_agent_denied_mutation() {
    let (handle, set) = fixture();
    call(
        &set,
        "lists.create",
        json!({"list":{"id":"a","name":"First"}}),
    );
    let input = json!({"list_id":"a","item":{"id":"one","text":"é"}});
    let added = invoke(
        &set,
        "items.add",
        input.clone(),
        PrincipalKind::Agent,
        false,
    )
    .unwrap();
    assert_eq!(
        added.result.unwrap()["items"][0],
        json!({"id":"one","text":"é","ticked":false})
    );
    assert_eq!(
        invoke(&set, "items.add", input, PrincipalKind::Human, false)
            .unwrap_err()
            .code,
        "item_exists"
    );
    let before = handle.get("a").unwrap();
    assert!(
        set.get("items.tick")
            .unwrap()
            .descriptor()
            .principals
            .contains(&PrincipalKind::Agent)
    );
    let refusal = invoke(
        &set,
        "items.tick",
        json!({"list_id":"a","id":"one"}),
        PrincipalKind::Agent,
        false,
    )
    .unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.evidence),
        ("not_admitted", Evidence::NotDispatched)
    );
    assert_eq!(handle.get("a"), Some(before.clone()));
    let ticked = list(call(&set, "items.tick", json!({"list_id":"a","id":"one"})));
    assert!(ticked.items[0].ticked);
    assert!(ticked.revision > before.revision);
    assert_eq!(
        list(call(&set, "items.tick", json!({"list_id":"a","id":"one"}))),
        ticked
    );
    assert_eq!(
        invoke(
            &set,
            "items.tick",
            json!({"list_id":"a","id":"missing"}),
            PrincipalKind::Human,
            false
        )
        .unwrap_err()
        .code,
        "item_not_found"
    );
}

#[test]
fn administrative_compaction_removes_only_ticked_items_and_delete_is_destructive() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    handle.add_item("a", "one", "Keep").unwrap();
    handle.add_item("a", "two", "Remove").unwrap();
    call(&set, "items.tick", json!({"list_id":"a","id":"two"}));
    let before = handle.snapshot();
    for (name, input) in [
        ("lists.compact", json!({})),
        ("lists.delete", json!({"id":"a"})),
    ] {
        assert_eq!(
            invoke(&set, name, input, PrincipalKind::Agent, false)
                .unwrap_err()
                .code,
            "not_admitted"
        );
        assert_eq!(handle.snapshot(), before);
    }
    assert_eq!(call(&set, "lists.compact", json!({})), json!({"removed":1}));
    let compacted = handle.get("a").unwrap();
    assert_eq!(
        compacted
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["one"]
    );
    assert!(compacted.revision > before[0].revision);
    assert_eq!(call(&set, "lists.compact", json!({})), json!({"removed":0}));
    assert_eq!(handle.get("a"), Some(compacted.clone()));
    let deleted = list(call(&set, "lists.delete", json!({"id":"a"})));
    assert_eq!(deleted.items, compacted.items);
    assert!(deleted.revision > compacted.revision);
    assert!(handle.get("a").is_none());
    assert_eq!(
        invoke(
            &set,
            "lists.delete",
            json!({"id":"a"}),
            PrincipalKind::Human,
            false
        )
        .unwrap_err()
        .code,
        "list_not_found"
    );
}

#[test]
fn deletion_and_recreation_do_not_allow_an_old_revision_fence_to_match() {
    let (handle, set) = fixture();
    let original = handle.create_list("a", "Original").unwrap();
    call(&set, "lists.delete", json!({"id":"a"}));
    let recreated = handle.create_list("a", "Recreated").unwrap();
    assert!(recreated.revision > original.revision);
    let refusal = invoke(
        &set,
        "lists.rename",
        json!({"id":"a","name":"Stale","revision":original.revision}),
        PrincipalKind::Human,
        false,
    )
    .unwrap_err();
    assert_eq!(refusal.code, "list_changed");
    assert_eq!(handle.get("a"), Some(recreated));
}

#[test]
fn fault_exercises_the_undeclared_refusal_contract_without_changing_state() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    let before = handle.snapshot();
    let refusal = invoke(&set, "lists.fault", json!({}), PrincipalKind::Human, false).unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.category),
        ("undeclared_refusal", Category::Internal)
    );
    assert_eq!(refusal.details.unwrap()["code"], "fixture_fault");
    assert_eq!(handle.snapshot(), before);
}

#[test]
fn export_streams_its_held_snapshot_with_backpressure_before_returning_a_count() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    handle.add_item("a", "one", "First").unwrap();
    handle.add_item("a", "two", "Second").unwrap();
    let mut held = handle.hold("lists.export").unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let output = records.clone();
    let (release, wait) = futures_channel::oneshot::channel::<()>();
    let wait = Mutex::new(Some(wait));
    let state = handle.clone();
    let sink = RecordSink::new(move |record| {
        assert!(!state.snapshot().is_empty());
        output.lock().unwrap().push(record);
        let wait = wait.lock().unwrap().take();
        async move {
            if let Some(wait) = wait {
                wait.await.unwrap();
            }
            Ok(())
        }
    });
    let selection = set.all();
    let mut future = pin!(set.call(
        &selection,
        Principal::new("list-reader", PrincipalKind::Human),
        "lists.export",
        json!({"id":"a"}),
        sink,
        Cancellation::new(),
        false
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert!(block_on(held.entered()));
    handle.add_item("a", "three", "Later").unwrap();
    assert!(held.release());
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert_eq!(records.lock().unwrap().len(), 1);
    release.send(()).unwrap();
    assert_eq!(block_on(future).unwrap().result, Some(json!({"count":2})));
    let records = records.lock().unwrap();
    assert_eq!(
        records
            .iter()
            .map(|record| record["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(handle.get("a").unwrap().items.len(), 3);
}

#[test]
fn an_empty_export_returns_zero_and_a_sink_refusal_keeps_its_code_and_evidence() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    assert_eq!(
        call(&set, "lists.export", json!({"id":"a"})),
        json!({"count":0})
    );
    handle.add_item("a", "one", "First").unwrap();
    let mut expected = Refusal::new(
        "record_delivery_failed",
        Category::Unavailable,
        "Delivery failed.",
    )
    .with_evidence(Evidence::Uncertain);
    expected.retryable = true;
    let failure = expected.clone();
    let sink = RecordSink::new(move |_| {
        let failure = failure.clone();
        async move { Err(failure) }
    });
    let refusal = block_on(set.call(
        &set.all(),
        Principal::new("list-reader", PrincipalKind::Human),
        "lists.export",
        json!({"id":"a"}),
        sink,
        Cancellation::new(),
        false,
    ))
    .unwrap_err();
    assert_eq!(refusal, expected);
}

#[test]
fn watch_subscribes_before_the_hold_delivers_live_changes_and_ends_on_cancellation() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    handle.create_list("b", "Other").unwrap();
    let mut held = handle.hold("lists.watch").unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let output = records.clone();
    let state = handle.clone();
    let (notice, mut noticed) = futures_channel::mpsc::unbounded();
    let sink = RecordSink::new(move |record| {
        assert!(state.get("a").is_some());
        output.lock().unwrap().push(record);
        notice.unbounded_send(()).unwrap();
        async { Ok(()) }
    });
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let selection = set.all();
    let watching = set.call(
        &selection,
        Principal::new("list-reader", PrincipalKind::Human),
        "lists.watch",
        json!({"id":"a"}),
        sink,
        cancel,
        false,
    );
    let script = async {
        assert!(held.entered().await);
        handle.rename_list("a", "While held").unwrap();
        handle.rename_list("b", "Invisible").unwrap();
        assert!(records.lock().unwrap().is_empty());
        assert!(held.release());
        noticed.next().await.unwrap();
        // The subscriber is now waiting: this write must wake the real executor.
        handle.rename_list("a", "After release").unwrap();
        noticed.next().await.unwrap();
        handle.add_item("a", "one", "New").unwrap();
        noticed.next().await.unwrap();
        signal.cancel();
    };
    let (result, ()) = block_on(join(watching, script));
    let refusal = result.unwrap_err();
    assert_eq!(
        (refusal.code.as_str(), refusal.evidence),
        ("cancelled", Evidence::Uncertain)
    );
    assert!(refusal.retryable);
    let changes: Vec<ListChange> = records
        .lock()
        .unwrap()
        .iter()
        .cloned()
        .map(|record| serde_json::from_value(record).unwrap())
        .collect();
    assert_eq!(
        changes.iter().map(|change| change.kind).collect::<Vec<_>>(),
        [
            ChangeKind::Renamed,
            ChangeKind::Renamed,
            ChangeKind::ItemAdded
        ]
    );
    assert!(changes.iter().all(|change| change.list.id == "a"));
    assert!(
        changes
            .windows(2)
            .all(|pair| pair[0].list.revision < pair[1].list.revision)
    );
    handle.rename_list("a", "After cancellation").unwrap();
    assert_eq!(records.lock().unwrap().len(), 3);
}

#[test]
fn held_calls_are_one_shot_cancel_safe_and_denied_calls_do_not_consume_them() {
    let (handle, set) = fixture();
    handle.create_list("a", "First").unwrap();
    handle.add_item("a", "one", "First").unwrap();
    assert!(handle.hold("unknown.operation").is_none());
    let mut held = handle.hold("items.tick").unwrap();
    assert!(handle.hold("items.tick").is_none());
    {
        let mut observation = pin!(held.entered());
        assert!(
            observation
                .as_mut()
                .poll(&mut Context::from_waker(&noop_waker()))
                .is_pending()
        );
    }
    assert_eq!(
        invoke(
            &set,
            "items.tick",
            json!({"list_id":"a","id":"one"}),
            PrincipalKind::Agent,
            false
        )
        .unwrap_err()
        .code,
        "not_admitted"
    );
    let selection = set.all();
    let mut future = pin!(set.call(
        &selection,
        Principal::new("list-reader", PrincipalKind::Human),
        "items.tick",
        json!({"list_id":"a","id":"one"}),
        RecordSink::default(),
        Cancellation::new(),
        false
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert!(block_on(held.entered()));
    assert!(block_on(held.entered()));
    assert!(held.release());
    assert!(list(block_on(future).unwrap().result.unwrap()).items[0].ticked);
    let held = handle.hold("lists.get").unwrap();
    drop(held);
    assert_eq!(call(&set, "lists.get", json!({"id":"a"}))["id"], "a");
}

#[test]
fn cancellation_at_a_write_hold_changes_nothing_and_drops_the_release_receiver() {
    let (handle, set) = fixture();
    let mut held = handle.hold("lists.create").unwrap();
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let selection = set.all();
    let mut future = pin!(set.call(
        &selection,
        Principal::new("list-reader", PrincipalKind::Human),
        "lists.create",
        json!({"list":{"id":"a","name":"First"}}),
        RecordSink::default(),
        cancel,
        false
    ));
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&noop_waker()))
            .is_pending()
    );
    assert!(block_on(held.entered()));
    signal.cancel();
    let refusal = block_on(future).unwrap_err();
    assert_eq!(refusal.evidence, Evidence::Uncertain);
    assert!(!held.release());
    assert!(handle.snapshot().is_empty());
}

#[test]
fn input_failures_missing_lists_and_independent_sets_keep_the_same_contract() {
    let (handle, set) = fixture();
    for operation in set
        .operations()
        .filter(|operation| !operation.name.as_str().starts_with("clapham."))
    {
        let refusal = invoke(
            &set,
            operation.name.as_str(),
            json!({"unexpected":true}),
            PrincipalKind::Human,
            false,
        )
        .unwrap_err();
        assert_eq!(refusal.code, "invalid_input", "{}", operation.name);
    }
    for (name, input) in [
        ("lists.get", json!({"id":"missing"})),
        ("lists.watch", json!({"id":"missing"})),
        ("lists.export", json!({"id":"missing"})),
        (
            "lists.rename",
            json!({"id":"missing","name":"New","revision":1}),
        ),
        (
            "items.add",
            json!({"list_id":"missing","item":{"id":"one","text":"First"}}),
        ),
        ("items.tick", json!({"list_id":"missing","id":"one"})),
    ] {
        assert_eq!(
            invoke(&set, name, input, PrincipalKind::Human, false)
                .unwrap_err()
                .code,
            "list_not_found"
        );
    }
    let other = compose_with_options(
        handle.contributions(),
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(ListsGate),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    call(
        &set,
        "lists.create",
        json!({"list":{"id":"a","name":"First"}}),
    );
    assert_eq!(
        call(&other, "lists.get", json!({"id":"a"})),
        call(&set, "lists.get", json!({"id":"a"}))
    );
}
