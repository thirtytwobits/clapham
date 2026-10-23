//! Built-in discovery is scoped to the authenticated selection and live admission policy.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use clapham::{
    Admission, ApplicationInfo, Bounds, Call, CallResult, Cancellation, Catalogue, Category,
    ComposeOptions, ComposedSet, CompositionCode, Contribution, Effect, Evidence, Handler,
    HandlerFuture, Locality, Operation, Outcome, Principal, PrincipalKind, RecordSink, Selection,
    VersionInfo, compose, compose_with_options,
};
use futures_executor::block_on;
use serde_json::{Value, json};

fn operation(name: &str, effect: Effect) -> Operation {
    serde_json::from_value(json!({
        "name": name, "description": "Read or maintain a list.",
        "input": { "type": "object", "properties": { "id": { "type": "string" } } },
        "output": { "type": "array", "items": { "type": "string" } },
        "effect": effect, "locality": "workspace", "idempotent": true, "open_world": false
    }))
    .unwrap()
}

fn contribution() -> Contribution {
    Contribution::new("lists")
        .operation(operation("lists.get", Effect::Query), |_| async {
            panic!("discovery must not dispatch the described operation")
        })
        .operation(
            operation("lists.compact", Effect::Administrative),
            |_| async { panic!("discovery must not dispatch the described operation") },
        )
}

fn call(
    set: &ComposedSet,
    selection: &Selection,
    kind: PrincipalKind,
    name: &str,
    input: Value,
) -> CallResult {
    block_on(set.call(
        selection,
        Principal::new("list-reader", kind),
        name,
        input,
        RecordSink::default(),
        Cancellation::new(),
        false,
    ))
}

fn describe(set: &ComposedSet, selection: &Selection, kind: PrincipalKind, input: Value) -> Value {
    let outcome = call(set, selection, kind, "clapham.describe", input).unwrap();
    assert_eq!(outcome.evidence, Evidence::Completed);
    assert!(!outcome.is_error);
    let result = outcome.result.unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/catalogue.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&result)
        .unwrap();
    result
}

fn names(value: Value) -> Vec<String> {
    let Catalogue::Operations(operations) = serde_json::from_value(value).unwrap() else {
        panic!("expected a catalogue array")
    };
    operations
        .into_iter()
        .map(|operation| operation.name.to_string())
        .collect()
}

#[test]
fn an_empty_host_gets_both_built_ins_in_the_registry_and_all_selection() {
    let set = compose([], Bounds::default()).unwrap();
    assert_eq!(
        set.operations()
            .map(|operation| operation.name.as_str())
            .collect::<Vec<_>>(),
        ["clapham.describe", "clapham.version"]
    );
    assert_eq!(set.all().operations().len(), 2);
    for operation in set.operations() {
        assert_eq!(operation.effect, Effect::Query);
        assert_eq!(operation.locality, Locality::Workspace);
        assert_eq!(operation.principals, Effect::Query.default_principals());
        assert!(operation.idempotent);
        assert!(!operation.open_world);
    }
    assert_eq!(set.all().revision(), set.revision());
    assert_eq!(
        names(describe(&set, &set.all(), PrincipalKind::Human, json!({}))),
        ["clapham.describe", "clapham.version"]
    );
}

#[test]
fn default_discovery_omits_administration_for_an_agent_but_lists_it_for_a_human() {
    let set = compose([contribution()], Bounds::default()).unwrap();
    assert_eq!(
        names(describe(&set, &set.all(), PrincipalKind::Agent, json!({}))),
        ["clapham.describe", "clapham.version", "lists.get"]
    );
    assert_eq!(
        names(describe(&set, &set.all(), PrincipalKind::Human, json!({}))),
        [
            "clapham.describe",
            "clapham.version",
            "lists.compact",
            "lists.get"
        ]
    );
}

#[test]
fn named_discovery_returns_the_exact_descriptor_including_its_schemas() {
    let set = compose([contribution()], Bounds::default()).unwrap();
    // Self-description also exercises schemas embedded inside the catalogue schema.
    for name in ["lists.get", "clapham.describe", "clapham.version"] {
        let result = describe(
            &set,
            &set.all(),
            PrincipalKind::Human,
            json!({ "name": name }),
        );
        assert_eq!(
            result,
            serde_json::to_value(set.get(name).unwrap().descriptor()).unwrap()
        );
        let Catalogue::Operation(operation) = serde_json::from_value(result).unwrap() else {
            panic!("a named lookup must return a descriptor")
        };
        assert_eq!(operation.as_ref(), set.get(name).unwrap().descriptor());
    }
}

#[test]
fn selection_membership_and_admission_hide_names_with_the_same_unknown_refusal() {
    let set = compose([contribution()], Bounds::default()).unwrap();
    let selected = set.select(["clapham.describe", "lists.compact"]).unwrap();
    assert_eq!(
        names(describe(&set, &selected, PrincipalKind::Agent, json!({}))),
        ["clapham.describe"]
    );
    assert_eq!(
        names(describe(&set, &selected, PrincipalKind::Human, json!({}))),
        ["clapham.describe", "lists.compact"]
    );
    let mut refusals = Vec::new();
    for name in [
        "lists.get",
        "lists.compact",
        "clapham.version",
        "lists.absent",
        "",
    ] {
        let refusal = call(
            &set,
            &selected,
            PrincipalKind::Agent,
            "clapham.describe",
            json!({ "name": name }),
        )
        .unwrap_err();
        assert_eq!(
            (refusal.code.as_str(), refusal.category),
            ("unknown_operation", Category::NotFound)
        );
        refusals.push(refusal);
    }
    assert!(refusals.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn a_custom_gate_checks_only_selected_candidates_and_hides_pending_operations() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let set = compose_with_options(
        [contribution()],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |principal: &Principal, operation: &Operation| {
                assert_eq!(principal.kind, PrincipalKind::Agent);
                recorded.lock().unwrap().push(operation.name.to_string());
                if operation.name.as_str() == "lists.get" {
                    Admission::Pending
                } else {
                    Admission::Allow
                }
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let selected = set
        .select(["clapham.describe", "lists.compact", "lists.get"])
        .unwrap();
    assert_eq!(
        names(describe(&set, &selected, PrincipalKind::Agent, json!({}))),
        ["clapham.describe", "lists.compact"]
    );
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "clapham.describe",
            "clapham.describe",
            "lists.compact",
            "lists.get"
        ]
    );
    seen.lock().unwrap().clear();
    let refusal = call(
        &set,
        &selected,
        PrincipalKind::Agent,
        "clapham.describe",
        json!({ "name": "lists.get" }),
    )
    .unwrap_err();
    assert_eq!(refusal.code, "unknown_operation");
    assert!(!refusal.retryable);
    assert_eq!(*seen.lock().unwrap(), ["clapham.describe", "lists.get"]);
    seen.lock().unwrap().clear();
    let refusal = call(
        &set,
        &selected,
        PrincipalKind::Agent,
        "clapham.describe",
        json!({ "name": "clapham.version" }),
    )
    .unwrap_err();
    assert_eq!(refusal.code, "unknown_operation");
    assert_eq!(*seen.lock().unwrap(), ["clapham.describe"]);
}

#[test]
fn discovery_rechecks_changing_gate_state_on_each_request() {
    let allowed = Arc::new(AtomicBool::new(false));
    let state = allowed.clone();
    let set = compose_with_options(
        [contribution()],
        Bounds::default(),
        ComposeOptions {
            gate: Arc::new(move |_: &Principal, operation: &Operation| {
                if operation.name.as_str() == "lists.compact" && !state.load(Ordering::SeqCst) {
                    Admission::Deny
                } else {
                    Admission::Allow
                }
            }),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let selection = set.all();
    assert!(
        !names(describe(&set, &selection, PrincipalKind::Agent, json!({})))
            .contains(&"lists.compact".into())
    );
    allowed.store(true, Ordering::SeqCst);
    assert!(
        names(describe(&set, &selection, PrincipalKind::Agent, json!({})))
            .contains(&"lists.compact".into())
    );
    assert_eq!(
        describe(
            &set,
            &selection,
            PrincipalKind::Agent,
            json!({"name": "lists.compact"})
        )["name"],
        "lists.compact"
    );
}

#[test]
fn the_outer_gate_can_deny_or_defer_discovery_before_any_candidate_is_examined() {
    for decision in [Admission::Deny, Admission::Pending] {
        let set = compose_with_options(
            [contribution()],
            Bounds::default(),
            ComposeOptions {
                gate: Arc::new(move |_: &Principal, operation: &Operation| {
                    assert_eq!(operation.name.as_str(), "clapham.describe");
                    decision
                }),
                ..ComposeOptions::default()
            },
        )
        .unwrap();
        let refusal = call(
            &set,
            &set.all(),
            PrincipalKind::Human,
            "clapham.describe",
            json!({}),
        )
        .unwrap_err();
        assert_eq!(
            refusal.code,
            if decision == Admission::Pending {
                "admission_pending"
            } else {
                "not_admitted"
            }
        );
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
    }
}

#[test]
fn built_ins_require_selection_and_accept_only_their_declared_inputs() {
    let set = compose([contribution()], Bounds::default()).unwrap();
    let selected = set.select(["lists.get"]).unwrap();
    for name in ["clapham.describe", "clapham.version"] {
        assert_eq!(
            call(&set, &selected, PrincipalKind::Human, name, json!({}))
                .unwrap_err()
                .code,
            "unknown_operation"
        );
    }
    for input in [
        json!({"name": null}),
        json!({"name": 1}),
        json!({"extra": true}),
        json!([]),
    ] {
        let refusal = call(
            &set,
            &set.all(),
            PrincipalKind::Human,
            "clapham.describe",
            input,
        )
        .unwrap_err();
        assert_eq!(refusal.code, "invalid_input");
        assert_eq!(refusal.evidence, Evidence::NotDispatched);
    }
    assert_eq!(
        call(
            &set,
            &set.all(),
            PrincipalKind::Human,
            "clapham.version",
            json!({"name": "lists.get"})
        )
        .unwrap_err()
        .code,
        "invalid_input"
    );
}

#[test]
fn version_reports_the_core_and_exact_application_metadata_from_composition() {
    let application = ApplicationInfo {
        name: "Lists: é".into(),
        version: "1.2.3+local".into(),
    };
    let set = compose_with_options(
        [],
        Bounds::default(),
        ComposeOptions {
            application: Some(application.clone()),
            ..ComposeOptions::default()
        },
    )
    .unwrap();
    let result = call(
        &set.clone(),
        &set.all(),
        PrincipalKind::Agent,
        "clapham.version",
        json!({}),
    )
    .unwrap()
    .result
    .unwrap();
    assert_eq!(
        result,
        json!({"clapham": env!("CARGO_PKG_VERSION"), "application": application})
    );
    let version: VersionInfo = serde_json::from_value(result).unwrap();
    assert_eq!(version.application, Some(application));
    let default = compose([], Bounds::default()).unwrap();
    assert_eq!(
        call(
            &default,
            &default.all(),
            PrincipalKind::Human,
            "clapham.version",
            json!({})
        )
        .unwrap()
        .result
        .unwrap(),
        json!({"clapham": env!("CARGO_PKG_VERSION")})
    );
    // Identity is result metadata; it does not alter the operation contract.
    assert_eq!(set.revision(), default.revision());
}

#[test]
fn built_in_operations_and_schemas_consume_the_same_finite_composition_budgets() {
    let set = compose([], Bounds::default()).unwrap();
    let descriptors = set.operations().collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&descriptors).unwrap().len();
    assert!(
        compose(
            [],
            Bounds {
                tools: 2,
                catalogue_bytes: bytes,
                ..Bounds::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        compose(
            [],
            Bounds {
                tools: 1,
                ..Bounds::default()
            }
        )
        .unwrap_err()
        .code,
        CompositionCode::TooManyOperations
    );
    assert_eq!(
        compose(
            [],
            Bounds {
                catalogue_bytes: bytes - 1,
                ..Bounds::default()
            }
        )
        .unwrap_err()
        .code,
        CompositionCode::CatalogueTooLarge
    );
    let schema_bytes = descriptors
        .iter()
        .map(|operation| {
            serde_json::to_vec(operation.output.as_ref().unwrap())
                .unwrap()
                .len()
        })
        .max()
        .unwrap();
    assert!(
        compose(
            [],
            Bounds {
                schema_bytes,
                ..Bounds::default()
            }
        )
        .is_ok()
    );
    assert_eq!(
        compose(
            [],
            Bounds {
                schema_bytes: schema_bytes - 1,
                ..Bounds::default()
            }
        )
        .unwrap_err()
        .code,
        CompositionCode::SchemaTooLarge
    );
}

#[test]
fn the_published_catalogue_schema_rejects_malformed_descriptors_and_result_shapes() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/catalogue.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let descriptor = serde_json::to_value(operation("lists.get", Effect::Query)).unwrap();
    for valid in [json!([]), descriptor.clone(), json!([descriptor.clone()])] {
        validator.validate(&valid).unwrap();
    }
    let mut malformed = descriptor;
    malformed["input"] = json!({"type": "array"});
    for invalid in [
        json!(null),
        json!(true),
        json!({"operations": []}),
        malformed.clone(),
        json!([malformed]),
    ] {
        assert!(!validator.is_valid(&invalid), "schema accepted {invalid}");
        assert!(serde_json::from_value::<Catalogue>(invalid).is_err());
    }
}

struct DropHandler(Arc<AtomicBool>);
impl Handler for DropHandler {
    fn call(&self, _: Call) -> HandlerFuture<'_> {
        Box::pin(async { Ok(Outcome::new(json!([]))) })
    }
}
impl Drop for DropHandler {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn the_description_snapshot_does_not_retain_unselected_handlers_or_form_a_cycle() {
    let dropped = Arc::new(AtomicBool::new(false));
    let set = compose(
        [Contribution::new("lists").operation(
            operation("lists.get", Effect::Query),
            DropHandler(dropped.clone()),
        )],
        Bounds::default(),
    )
    .unwrap();
    let selected = set.select(["clapham.describe"]).unwrap();
    drop(set);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(selected.operations().len(), 1);
}

#[test]
fn low_level_discovery_uses_the_whole_catalogue_but_still_filters_admission() {
    let set = compose([contribution()], Bounds::default()).unwrap();
    let outcome = block_on(set.get("clapham.describe").unwrap().invoke(Call::new(
        "clapham.describe".parse().unwrap(),
        Principal::new("list-reader", PrincipalKind::Agent),
        json!({}),
    )))
    .unwrap();
    assert_eq!(
        names(outcome.result.unwrap()),
        ["clapham.describe", "clapham.version", "lists.get"]
    );
}
