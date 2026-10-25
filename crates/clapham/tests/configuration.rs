//! Shared JSON/Rust composition, typed handlers and OAuth permission contracts.

use clapham::{
    Bounds, Call, Category, Configuration, Contribution, Effect, Exposure, OAuthContext, Refusal,
    ResourcePolicy, Source, TypedOperation,
};
use futures_executor::block_on;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetList {
    #[schemars(length(min = 1))]
    id: String,
}

fn operation() -> TypedOperation<GetList, String> {
    TypedOperation::new("lists.get", "Read a list.", Effect::Query)
        .unwrap()
        .require_scope("lists:read")
        .policy("lists.can_read")
        .refusal("list_access_denied", Category::Refused)
}

fn provider() -> Contribution {
    Contribution::new("lists").typed_operation(
        operation(),
        |call: Call, input: GetList| async move {
            assert_eq!(call.name.as_str(), "lists.get");
            Ok(input.id)
        },
    )
}

fn policy() -> ResourcePolicy {
    ResourcePolicy::typed(|auth: OAuthContext, input: GetList| async move {
        if auth.subject() == "alice"
            && auth
                .claim("list_ids")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|ids| ids.iter().any(|id| id == &input.id))
        {
            Ok(())
        } else {
            Err(Refusal::new(
                "list_access_denied",
                Category::Refused,
                "the grant cannot read this list",
            ))
        }
    })
}

fn authority(scopes: &[&str]) -> OAuthContext {
    OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        "alice",
        scopes.iter().copied(),
    )
    .with_client("assistant")
    .with_claim("list_ids", serde_json::json!(["tasks"]))
}

const JSON: &str = r#"{
    "oauth":{"issuer":"https://identity.example.com","audience":"https://lists.example.com"},
    "sources":{"local":{"provider":"lists"}},
    "operations":[{"source":"local","name":"lists.get","require":{"scopes":["application:access"],"policy":"lists.can_read"}}]
}"#;

fn registered(config: Configuration) -> Configuration {
    config
        .register_provider("lists", provider())
        .register_policy("lists.can_read", policy())
}

fn rust() -> Configuration {
    Configuration::new("https://identity.example.com", "https://lists.example.com")
        .source("local", Source::provider("lists"))
        .expose(
            Exposure::typed("local", &operation())
                .require_scope("application:access")
                .policy("lists.can_read"),
        )
}

#[test]
fn json_rust_and_mixed_configuration_share_definitions_permissions_and_revisions() {
    let json = registered(Configuration::from_json(JSON).unwrap())
        .compose(Bounds::default())
        .unwrap();
    let rust = registered(rust()).compose(Bounds::default()).unwrap();
    let mut seed: serde_json::Value = serde_json::from_str(JSON).unwrap();
    seed["operations"] = serde_json::json!([]);
    let mixed = registered(
        Configuration::from_json(&seed.to_string()).unwrap().expose(
            Exposure::new("local", "lists.get")
                .policy("lists.can_read")
                .require_scope("application:access"),
        ),
    )
    .compose(Bounds::default())
    .unwrap();
    for set in [&json, &rust, &mixed] {
        assert_eq!(set.revision(), json.revision());
        assert_eq!(
            set.get("lists.get").unwrap().descriptor().scopes,
            ["application:access", "lists:read"]
        );
        assert_eq!(
            block_on(operation().call(
                set,
                authority(&["application:access", "lists:read"]),
                GetList { id: "tasks".into() }
            ))
            .unwrap(),
            "tasks"
        );
        let refused = block_on(operation().call(
            set,
            authority(&["application:access"]),
            GetList { id: "tasks".into() },
        ))
        .unwrap_err();
        assert_eq!(refused.code, "insufficient_scope");
        assert_eq!(refused.evidence, clapham::Evidence::NotDispatched);
        let refused = block_on(operation().call(
            set,
            authority(&["application:access", "lists:read"]),
            GetList {
                id: "private".into(),
            },
        ))
        .unwrap_err();
        assert_eq!(refused.code, "list_access_denied");
        assert_eq!(refused.evidence, clapham::Evidence::NotDispatched);
    }
}

#[test]
fn equivalent_grants_have_equivalent_access_and_resource_binding_is_checked() {
    let set = registered(rust()).compose(Bounds::default()).unwrap();
    let grant = authority(&["lists:read", "application:access"][..]);
    let input = GetList { id: "tasks".into() };
    assert_eq!(
        block_on(operation().call(
            &set,
            grant.clone().with_client("interactive"),
            input.clone()
        ))
        .unwrap(),
        block_on(operation().call(&set, grant, input.clone())).unwrap()
    );
    let wrong = OAuthContext::from_verified(
        "https://other.example.com",
        "https://lists.example.com",
        "alice",
        ["lists:read", "application:access"],
    );
    assert_eq!(
        block_on(operation().call(&set, wrong, input.clone()))
            .unwrap_err()
            .code,
        "not_authenticated"
    );
    let missing = authority(&["lists:read"][..]);
    assert_eq!(
        block_on(operation().call(&set, missing, input))
            .unwrap_err()
            .code,
        "insufficient_scope"
    );
}

#[test]
fn json_rust_and_mixed_configuration_reject_the_same_semantic_errors() {
    for mode in 0..3 {
        let config = || {
            if mode == 0 {
                Configuration::from_json(JSON).unwrap()
            } else if mode == 1 {
                rust()
            } else {
                let mut json: serde_json::Value = serde_json::from_str(JSON).unwrap();
                json["operations"] = serde_json::json!([]);
                Configuration::from_json(&json.to_string()).unwrap().expose(
                    Exposure::new("local", "lists.get")
                        .require_scope("application:access")
                        .policy("lists.can_read"),
                )
            }
        };
        assert!(
            registered(config())
                .expose(Exposure::new("local", "lists.get"))
                .compose(Bounds::default())
                .is_err()
        );
        assert!(
            registered(config())
                .expose(Exposure::new("absent", "lists.get"))
                .compose(Bounds::default())
                .is_err()
        );
        let mut invalid = config();
        invalid.operations[0].require.policy = Some("absent".into());
        assert!(registered(invalid).compose(Bounds::default()).is_err());
        let mut invalid = config();
        invalid.operations[0].require.policy = Some("incompatible".into());
        assert!(
            registered(invalid)
                .register_policy(
                    "incompatible",
                    ResourcePolicy::typed(|_: OAuthContext, _: u64| async { Ok(()) })
                )
                .compose(Bounds::default())
                .is_err()
        );
        let mut invalid = config();
        invalid.operations[0].expose_as = Some("bad alias".into());
        assert!(registered(invalid).compose(Bounds::default()).is_err());
        assert!(
            registered(config())
                .compose(Bounds {
                    schema_bytes: 1,
                    ..Bounds::default()
                })
                .is_err()
        );

        assert!(
            config()
                .register_policy("lists.can_read", policy())
                .compose(Bounds::default())
                .is_err()
        );
        let mut other = operation().descriptor().clone();
        other.name = "lists.other".parse().unwrap();
        let duplicate = config()
            .register_provider(
                "lists",
                provider().operation(other, |_: Call| async {
                    Ok(clapham::Outcome::new(serde_json::json!("tasks")))
                }),
            )
            .register_policy("lists.can_read", policy())
            .expose(Exposure::new("local", "lists.other").alias("lists.get"));
        assert!(
            duplicate
                .compose(Bounds::default())
                .unwrap_err()
                .to_string()
                .contains("duplicate exposed name")
        );
        let mut malformed = operation().descriptor().clone();
        malformed
            .input
            .insert("minProperties".into(), serde_json::json!(-1));
        let malformed = config()
            .register_provider(
                "lists",
                Contribution::new("lists").operation(malformed, |_: Call| async {
                    Ok(clapham::Outcome::new(serde_json::json!("tasks")))
                }),
            )
            .register_policy("lists.can_read", policy());
        assert_eq!(
            malformed.compose(Bounds::default()).unwrap_err().code,
            clapham::CompositionCode::InvalidSchema
        );
        let mut invalid = config();
        invalid.operations[0].require.scopes = vec!["bad scope".into()];
        assert!(registered(invalid).compose(Bounds::default()).is_err());
        let mut reduced = config();
        reduced.operations[0].require = Default::default();
        let reduced = registered(reduced).compose(Bounds::default()).unwrap();
        for asserted in [
            authority(&[]),
            authority(&[]).with_client("operator"),
            authority(&[]).with_claim("grant_profile", serde_json::json!("interactive")),
        ] {
            let refused =
                block_on(operation().call(&reduced, asserted, GetList { id: "tasks".into() }))
                    .unwrap_err();
            assert_eq!(refused.code, "insufficient_scope");
        }
        let refused = block_on(operation().call(
            &reduced,
            authority(&["lists:read"]),
            GetList {
                id: "private".into(),
            },
        ))
        .unwrap_err();
        assert_eq!(refused.code, "list_access_denied");
    }
    assert!(
        Configuration::from_json(&JSON.replace(
            "\"local\":{\"provider\":\"lists\"}",
            "\"local\":{\"provider\":\"lists\"},\"local\":{\"provider\":\"lists\"}"
        ))
        .is_err()
    );
}

#[test]
fn composition_aliases_preserve_intrinsic_requirements_and_original_handler_names() {
    let set = registered(
        Configuration::new("https://identity.example.com", "https://lists.example.com")
            .source("local", Source::provider("lists"))
            .expose(Exposure::new("local", "lists.get").alias("selected.read")),
    )
    .compose(Bounds::default())
    .unwrap();
    let input = serde_json::json!({"id":"tasks"});
    let invoke = |auth| {
        block_on(set.call(
            &set.all(),
            auth,
            "selected.read",
            input.clone(),
            Default::default(),
            Default::default(),
            false,
        ))
    };
    assert_eq!(
        invoke(authority(&[])).unwrap_err().code,
        "insufficient_scope"
    );
    assert_eq!(
        invoke(authority(&["lists:read"][..]))
            .unwrap()
            .result
            .unwrap(),
        "tasks"
    );
    let result = block_on(set.call(
        &set.all(),
        authority(&[]),
        "clapham.describe",
        serde_json::json!({}),
        Default::default(),
        Default::default(),
        false,
    ))
    .unwrap();
    assert!(
        !result
            .result
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|descriptor| descriptor["name"] == "selected.read")
    );
}

#[test]
fn typed_authoring_preserves_schema_constraints_refusals_and_conversion_failures() {
    let op = TypedOperation::<GetList, u64>::new("lists.count", "Count lists.", Effect::Query)
        .unwrap()
        .refusal("list_busy", Category::Unavailable);
    let set = clapham::compose(
        [Contribution::new("lists").typed_operation(
            op.clone(),
            |_: Call, input: GetList| async move {
                if input.id == "busy" {
                    Err(Refusal::new("list_busy", Category::Internal, "busy"))
                } else {
                    Ok(2)
                }
            },
        )],
        Bounds::default(),
    )
    .unwrap();
    assert_eq!(
        block_on(op.call(&set, authority(&[]), GetList { id: "tasks".into() })).unwrap(),
        2
    );
    assert_eq!(
        block_on(op.call(&set, authority(&[]), GetList { id: "".into() }))
            .unwrap_err()
            .code,
        "invalid_input"
    );
    let refusal =
        block_on(op.call(&set, authority(&[]), GetList { id: "busy".into() })).unwrap_err();
    assert_eq!(refusal.code, "list_busy");
    assert_eq!(refusal.category, Category::Unavailable);
    #[derive(Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Empty {}
    let count = TypedOperation::<Empty, u64>::new("lists.empty", "Count all lists.", Effect::Query)
        .unwrap();
    let set = clapham::compose(
        [Contribution::new("lists")
            .typed_operation(count.clone(), |_: Call, _: Empty| async { Ok(2) })],
        Bounds::default(),
    )
    .unwrap();
    assert_eq!(
        block_on(count.call(&set, authority(&[]), Empty {})).unwrap(),
        2
    );
    assert_eq!(
        block_on(set.call(
            &set.all(),
            authority(&[]),
            "lists.empty",
            serde_json::json!({"extra":1}),
            Default::default(),
            Default::default(),
            false
        ))
        .unwrap_err()
        .code,
        "invalid_input"
    );
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Nested {
    #[serde(rename = "listId")]
    id: String,
    items: Vec<NestedItem>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NestedItem {
    #[schemars(length(min = 1))]
    text: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct RenamedOutput {
    #[serde(rename(serialize = "itemCount", deserialize = "itemCount"))]
    count: usize,
}

#[test]
fn typed_schemas_preserve_nested_references_renaming_and_unknown_field_rules() {
    let op = TypedOperation::<Nested, RenamedOutput>::new(
        "lists.count",
        "Count nested items.",
        Effect::Query,
    )
    .unwrap();
    assert!(op.descriptor().input["$defs"].get("NestedItem").is_some());
    assert!(op.descriptor().input["properties"].get("listId").is_some());
    let set = clapham::compose(
        [Contribution::new("lists").typed_operation(
            op.clone(),
            |_: Call, input: Nested| async move {
                Ok(RenamedOutput {
                    count: input.items.len(),
                })
            },
        )],
        Bounds::default(),
    )
    .unwrap();
    let input = || Nested {
        id: "tasks".into(),
        items: vec![NestedItem {
            text: "Read".into(),
        }],
    };
    assert_eq!(
        block_on(op.call(&set, authority(&[]), input()))
            .unwrap()
            .count,
        1
    );
    for invalid in [
        serde_json::json!({"listId":"tasks","items":[{"text":""}]}),
        serde_json::json!({"listId":"tasks","items":[{"text":"Read","extra":true}]}),
        serde_json::json!({"id":"tasks","items":[]}),
    ] {
        let refusal = block_on(set.call(
            &set.all(),
            authority(&[]),
            "lists.count",
            invalid,
            Default::default(),
            Default::default(),
            false,
        ))
        .unwrap_err();
        assert_eq!(refusal.code, "invalid_input");
        assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(try_from = "GetList")]
struct ConvertedInput {
    _id: String,
}
impl TryFrom<GetList> for ConvertedInput {
    type Error = &'static str;
    fn try_from(value: GetList) -> Result<Self, Self::Error> {
        if value.id == "blocked" {
            Err("blocked input")
        } else {
            Ok(Self { _id: value.id })
        }
    }
}
#[derive(JsonSchema)]
struct BrokenOutput {}
impl Serialize for BrokenOutput {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("scripted conversion failure"))
    }
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct PositiveCount {
    #[schemars(range(min = 1))]
    count: u64,
}

#[test]
fn typed_conversion_failures_preserve_dispatch_evidence_and_output_validation() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = calls.clone();
    let input = TypedOperation::<ConvertedInput, u64>::new(
        "lists.input",
        "Validate converted input.",
        Effect::Query,
    )
    .unwrap();
    let output = TypedOperation::<GetList, BrokenOutput>::new(
        "lists.output",
        "Fail serialisation after an effect.",
        Effect::Mutation,
    )
    .unwrap();
    let invalid = TypedOperation::<GetList, PositiveCount>::new(
        "lists.invalid",
        "Validate typed output constraints.",
        Effect::Query,
    )
    .unwrap();
    let set = clapham::compose(
        [Contribution::new("lists")
            .typed_operation(input, move |_: Call, _: ConvertedInput| {
                entered.fetch_add(1, Ordering::SeqCst);
                async { Ok(1) }
            })
            .typed_operation_with_outcome(output, |_: Call, _: GetList| async {
                let mut result = clapham::TypedOutcome::new(BrokenOutput {});
                result.evidence = clapham::Evidence::Uncertain;
                Ok(result)
            })
            .typed_operation(invalid, |_: Call, _: GetList| async {
                Ok(PositiveCount { count: 0 })
            })],
        Bounds::default(),
    )
    .unwrap();
    for (name, id, evidence, code) in [
        (
            "lists.input",
            "blocked",
            clapham::Evidence::NotDispatched,
            "invalid_input",
        ),
        (
            "lists.output",
            "tasks",
            clapham::Evidence::Uncertain,
            "invalid_result",
        ),
        (
            "lists.invalid",
            "tasks",
            clapham::Evidence::Completed,
            "invalid_result",
        ),
    ] {
        let refusal = block_on(set.call(
            &set.all(),
            authority(&[]),
            name,
            serde_json::json!({"id":id}),
            Default::default(),
            Default::default(),
            false,
        ))
        .unwrap_err();
        assert_eq!(refusal.code, code);
        assert_eq!(refusal.evidence, evidence);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn typed_outcomes_preserve_content_error_status_and_evidence() {
    let op = TypedOperation::<GetList, String>::new(
        "lists.details",
        "Return adapter content.",
        Effect::Query,
    )
    .unwrap();
    let set = clapham::compose(
        [Contribution::new("lists").typed_operation_with_outcome(
            op.clone(),
            |_: Call, input: GetList| async move {
                let mut result = clapham::TypedOutcome::new(input.id)
                    .with_content([serde_json::json!({"type":"text","text":"Details"})])
                    .unwrap();
                result.is_error = true;
                result.evidence = clapham::Evidence::Uncertain;
                Ok(result)
            },
        )],
        Bounds::default(),
    )
    .unwrap();
    let result =
        block_on(op.call_outcome(&set, authority(&[]), GetList { id: "tasks".into() })).unwrap();
    assert_eq!(result.result, "tasks");
    assert!(result.is_error);
    assert_eq!(result.evidence, clapham::Evidence::Uncertain);
    assert_eq!(result.content.unwrap()[0]["text"], "Details");
}

#[test]
fn resource_policy_cancellation_stops_before_handler_dispatch() {
    use std::{
        future::Future,
        pin::pin,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Context,
    };
    let (entered, signal) = futures_channel::oneshot::channel();
    let entered = std::sync::Mutex::new(Some(entered));
    let calls = Arc::new(AtomicUsize::new(0));
    let dispatched = calls.clone();
    let policy = ResourcePolicy::typed(move |_: OAuthContext, _: GetList| {
        let signal = entered.lock().unwrap().take().unwrap();
        async move {
            signal.send(()).unwrap();
            std::future::pending::<Result<(), Refusal>>().await
        }
    });
    let op =
        TypedOperation::<GetList, u64>::new("lists.get", "Check a held policy.", Effect::Query)
            .unwrap()
            .policy("lists.held");
    let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .provider(
            "lists",
            Contribution::new("lists").typed_operation(op, move |_: Call, _: GetList| {
                dispatched.fetch_add(1, Ordering::SeqCst);
                async { Ok(1) }
            }),
        )
        .register_policy("lists.held", policy)
        .expose(Exposure::new("lists", "lists.get"))
        .compose(Bounds::default())
        .unwrap();
    let cancel = clapham::Cancellation::new();
    let selection = set.all();
    let mut call = pin!(set.call(
        &selection,
        authority(&[]),
        "lists.get",
        serde_json::json!({"id":"tasks"}),
        Default::default(),
        cancel.clone(),
        false
    ));
    let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
    assert!(call.as_mut().poll(&mut cx).is_pending());
    block_on(signal).unwrap();
    cancel.cancel();
    let refusal = block_on(call).unwrap_err();
    assert_eq!(refusal.code, "cancelled");
    assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn configuration_resolves_remote_registrations_and_preserves_binding_revisions() {
    let remote = || {
        Contribution::new("lists")
            .typed_operation(
                TypedOperation::<GetList, String>::new(
                    "lists.get",
                    "Get a remote list.",
                    Effect::Query,
                )
                .unwrap()
                .require_scope("lists:read"),
                |_: Call, input: GetList| async move { Ok(input.id) },
            )
            .with_binding_revision("pinned-remote-v1")
    };
    let json = r#"{"oauth":{"issuer":"https://identity.example.com","audience":"https://lists.example.com"},"sources":{"upstream":{"endpoint":"https://upstream.example.com/mcp","credential":"upstream-grant"}},"operations":[{"source":"upstream","name":"lists.get","expose_as":"selected.read"}]}"#;
    let rust = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .source(
            "upstream",
            Source::remote("https://upstream.example.com/mcp", "upstream-grant"),
        )
        .expose(Exposure::new("upstream", "lists.get").alias("selected.read"));
    let json = Configuration::from_json(json).unwrap();
    let compose = |config: Configuration| {
        config
            .register_connection(
                "https://upstream.example.com/mcp",
                "upstream-grant",
                remote(),
            )
            .compose(Bounds::default())
            .unwrap()
    };
    let json = compose(json);
    let rust = compose(rust);
    assert_eq!(json.revision(), rust.revision());
    assert_eq!(json.all().revision(), rust.all().revision());
}

#[test]
fn typed_handlers_cannot_pass_domain_refusals_off_as_conversion_failures() {
    let op = TypedOperation::<GetList, u64>::new(
        "lists.refuse",
        "Refuse without a declared code.",
        Effect::Query,
    )
    .unwrap();
    let set = clapham::compose(
        [
            Contribution::new("lists").typed_operation(op.clone(), |_: Call, _: GetList| async {
                Err(Refusal::new(
                    "invalid_input",
                    Category::InvalidInput,
                    "application refusal",
                ))
            }),
        ],
        Bounds::default(),
    )
    .unwrap();
    let refusal =
        block_on(op.call(&set, authority(&[]), GetList { id: "tasks".into() })).unwrap_err();
    assert_eq!(refusal.code, "undeclared_refusal");
    assert_eq!(refusal.category, Category::Internal);
    assert_eq!(refusal.evidence, clapham::Evidence::Completed);
    assert_eq!(refusal.details.unwrap()["code"], "invalid_input");
}

#[test]
fn resource_policy_refusals_obey_declarations_and_preserve_not_dispatched_evidence() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for (code, declared) in [
        ("list_access_denied", true),
        ("cancelled", false),
        ("invalid_input", false),
        ("cancelled", true),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = calls.clone();
        let mut op = TypedOperation::<GetList, u64>::new(
            "lists.get",
            "Read a protected list.",
            Effect::Query,
        )
        .unwrap()
        .policy("lists.access");
        if declared {
            op = op.refusal(code, Category::Refused);
        }
        let policy = ResourcePolicy::typed(move |_: OAuthContext, _: GetList| async move {
            let mut refusal = Refusal::new(code, Category::Internal, "denied")
                .with_evidence(clapham::Evidence::Uncertain);
            refusal.retryable = true;
            refusal.details = Some(serde_json::json!({"id":"tasks"}));
            Err(refusal)
        });
        let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
            .provider(
                "lists",
                Contribution::new("lists").typed_operation(
                    op.clone(),
                    move |_: Call, _: GetList| {
                        entered.fetch_add(1, Ordering::SeqCst);
                        async { Ok(1) }
                    },
                ),
            )
            .register_policy("lists.access", policy)
            .expose(Exposure::typed("lists", &op))
            .compose(Bounds::default())
            .unwrap();
        let refusal =
            block_on(op.call(&set, authority(&[]), GetList { id: "tasks".into() })).unwrap_err();
        assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if declared {
            assert_eq!(refusal.code, code);
            assert_eq!(refusal.category, Category::Refused);
            assert!(refusal.retryable);
            assert_eq!(refusal.details.unwrap()["id"], "tasks");
        } else {
            assert_eq!(refusal.code, "undeclared_refusal");
            let original = refusal.details.unwrap();
            assert_eq!(original["code"], code);
            assert_eq!(original["evidence"], "not_dispatched");
        }
    }
}

#[test]
fn policy_input_conversion_failure_remains_a_dispatcher_error() {
    let op =
        TypedOperation::<GetList, u64>::new("lists.get", "Read a protected list.", Effect::Query)
            .unwrap()
            .policy("lists.convert");
    let set = Configuration::new("https://identity.example.com", "https://lists.example.com")
        .provider(
            "lists",
            Contribution::new("lists").typed_operation(op.clone(), |_: Call, _: GetList| async {
                panic!("handler must not run")
            }),
        )
        .register_policy(
            "lists.convert",
            ResourcePolicy::typed(|_: OAuthContext, _: ConvertedInput| async {
                panic!("policy must not run")
            }),
        )
        .expose(Exposure::typed("lists", &op))
        .compose(Bounds::default())
        .unwrap();
    let refusal = block_on(op.call(
        &set,
        authority(&[]),
        GetList {
            id: "blocked".into(),
        },
    ))
    .unwrap_err();
    assert_eq!(refusal.code, "invalid_input");
    assert_eq!(refusal.evidence, clapham::Evidence::NotDispatched);
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NamedList {
    id: String,
    title: String,
}

#[test]
fn policies_accept_schema_annotations_but_reject_validation_and_default_changes() {
    let op = TypedOperation::<NamedList, u64>::new("lists.get", "Read a list.", Effect::Query)
        .unwrap()
        .policy("lists.access");
    let config = |descriptor: clapham::Operation| {
        Configuration::new("https://identity.example.com", "https://lists.example.com")
            .provider(
                "lists",
                Contribution::new("lists").operation(descriptor, |_: Call| async {
                    Ok(clapham::Outcome::new(serde_json::json!(1)))
                }),
            )
            .register_policy(
                "lists.access",
                ResourcePolicy::typed(|_: OAuthContext, input: NamedList| async move {
                    assert_eq!(
                        (input.id.as_str(), input.title.as_str()),
                        ("tasks", "Tasks")
                    );
                    Ok(())
                }),
            )
            .expose(Exposure::typed("lists", &op))
    };
    let mut descriptor = op.descriptor().clone();
    descriptor.input.remove("$schema");
    descriptor
        .input
        .insert("title".into(), serde_json::json!("RemoteList"));
    descriptor.input.insert(
        "description".into(),
        serde_json::json!("Remote description."),
    );
    descriptor.input["properties"]["title"]["description"] = serde_json::json!("A display title.");
    let set = config(descriptor.clone())
        .compose(Bounds::default())
        .unwrap();
    block_on(set.call(
        &set.all(),
        authority(&[]),
        "lists.get",
        serde_json::json!({"id":"tasks","title":"Tasks"}),
        Default::default(),
        Default::default(),
        false,
    ))
    .unwrap();
    for keyword in ["type", "default"] {
        let mut invalid = descriptor.clone();
        invalid.input["properties"]["title"][keyword] = if keyword == "type" {
            serde_json::json!("integer")
        } else {
            serde_json::json!("Other")
        };
        assert!(config(invalid).compose(Bounds::default()).is_err());
    }
    let mut other_dialect = descriptor;
    other_dialect.input.insert(
        "$schema".into(),
        serde_json::json!("http://json-schema.org/draft-07/schema#"),
    );
    assert!(config(other_dialect).compose(Bounds::default()).is_err());
}
