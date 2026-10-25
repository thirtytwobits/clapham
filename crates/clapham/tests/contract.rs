//! Behaviour and wire-schema agreement of the operation contract.

use clapham::{Category, Effect, Locality, Operation, OperationName, Refusal};
use serde_json::{Value, json};

fn descriptor() -> Value {
    json!({
        "name": "lists.items.get",
        "title": "Read an item",
        "description": "Read an item from a list.",
        "input": {
            "type": "object", "properties": { "item": { "type": "string" } },
            "required": ["item"], "additionalProperties": false
        },
        "output": { "type": "object", "properties": { "id": { "type": "string" } } },
        "effect": "query", "scopes": ["lists:read"], "policies": [], "confirmation": false,
        "locality": "workspace", "refusals": { "list_missing": "not_found", "item_conflict": "conflict" },
        "streams": true, "preview": false,
        "positional": ["list", "item"], "idempotent": false, "open_world": false
    })
}

fn minimal(effect: Effect) -> Value {
    json!({
        "name": "lists.items.get", "description": "Read an item.",
        "input": { "type": "object" }, "effect": effect,
        "locality": "daemon", "idempotent": true, "open_world": false
    })
}

fn operation_validator() -> jsonschema::Validator {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/operation.schema.json")).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

#[test]
fn descriptor_round_trips_and_validates_against_the_published_schema() {
    let wire = descriptor();
    let operation: Operation = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(operation.name.namespace(), Some("lists"));
    assert_eq!(operation.name.local_name(), "items.get");
    assert_eq!(operation.locality, Locality::Workspace);
    assert_eq!(operation.positional, ["list", "item"]);
    assert_eq!(serde_json::to_value(&operation).unwrap(), wire);
    operation_validator().validate(&wire).unwrap();
}

#[test]
fn refuses_missing_required_fields_in_rust_and_the_schema() {
    let validator = operation_validator();
    for field in [
        "name",
        "description",
        "input",
        "effect",
        "locality",
        "idempotent",
        "open_world",
    ] {
        let mut wire = minimal(Effect::Query);
        wire.as_object_mut().unwrap().remove(field);
        let error = serde_json::from_value::<Operation>(wire.clone()).unwrap_err();
        assert!(error.to_string().contains(field), "{error}");
        assert!(
            !validator.is_valid(&wire),
            "schema admitted a descriptor without {field}"
        );
    }
}

#[test]
fn refuses_invalid_names_at_both_wire_boundaries() {
    let validator = operation_validator();
    for name in [
        "".to_owned(),
        "a".repeat(129),
        "lists.item/add".into(),
        "lists:items".into(),
        "lists.é".into(),
        "lists.项目".into(),
        "lists.items\n".into(),
        "lists.\0item".into(),
        "lists. item".into(),
    ] {
        assert!(name.parse::<OperationName>().is_err(), "accepted {name:?}");
        assert!(serde_json::from_value::<OperationName>(json!(name)).is_err());
        let mut wire = minimal(Effect::Query);
        wire["name"] = json!(name);
        assert!(serde_json::from_value::<Operation>(wire.clone()).is_err());
        assert!(!validator.is_valid(&wire), "schema admitted {name:?}");
    }
}

#[test]
fn name_parser_and_schema_agree_on_the_whole_byte_alphabet() {
    let schema = serde_json::to_value(schemars::schema_for!(OperationName)).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for byte in 0..=255u8 {
        let name = char::from(byte).to_string();
        let expected = byte.is_ascii_alphanumeric() || b"_-.".contains(&byte);
        assert_eq!(
            name.parse::<OperationName>().is_ok(),
            expected,
            "parser: {byte}"
        );
        assert_eq!(validator.is_valid(&json!(name)), expected, "schema: {byte}");
    }
    for name in [
        "a".to_owned(),
        "a".repeat(128),
        "Lists_1.items-get.more".into(),
        ".".into(),
    ] {
        let parsed = OperationName::new(&name).unwrap();
        assert_eq!(parsed.as_str(), name);
        assert_eq!(parsed.to_string(), name);
        assert!(validator.is_valid(&json!(name)));
    }
    let bare: OperationName = "items".parse().unwrap();
    assert_eq!(bare.namespace(), None);
    assert_eq!(bare.local_name(), "items");
}

#[test]
fn requires_an_input_schema_with_an_explicit_object_root() {
    let validator = operation_validator();
    for input in [
        json!(null),
        json!(true),
        json!(17),
        json!([]),
        json!({}),
        json!({ "type": "array" }),
        json!({ "type": ["object", "null"] }),
    ] {
        let mut wire = minimal(Effect::Query);
        wire["input"] = input;
        assert!(
            serde_json::from_value::<Operation>(wire.clone()).is_err(),
            "accepted {wire}"
        );
        assert!(!validator.is_valid(&wire), "schema accepted {wire}");
    }
}

#[test]
fn accepts_object_or_boolean_output_schemas_and_nullable_optional_fields() {
    let validator = operation_validator();
    for output in [
        json!({}),
        json!({ "type": "string" }),
        json!(true),
        json!(false),
        json!(null),
    ] {
        let mut wire = minimal(Effect::Query);
        wire["output"] = output;
        wire["title"] = Value::Null;
        assert!(
            serde_json::from_value::<Operation>(wire.clone()).is_ok(),
            "refused {wire}"
        );
        validator.validate(&wire).unwrap();
    }
    for output in [json!(17), json!("object"), json!([])] {
        let mut wire = minimal(Effect::Query);
        wire["output"] = output;
        assert!(serde_json::from_value::<Operation>(wire.clone()).is_err());
        assert!(!validator.is_valid(&wire));
    }
}

#[test]
fn refuses_malformed_and_unknown_descriptor_fields() {
    let validator = operation_validator();
    for (field, value) in [
        ("scopes", Value::Null),
        ("scopes", json!([false])),
        ("effect", json!("unknown")),
        ("locality", json!("remote")),
        ("refusals", json!({ "item_conflict": "unknown" })),
        ("description", Value::Null),
        ("open_world", Value::Null),
        ("openWorld", json!(true)),
        ("positional", json!([17])),
    ] {
        let mut wire = minimal(Effect::Query);
        wire[field] = value;
        assert!(
            serde_json::from_value::<Operation>(wire.clone()).is_err(),
            "accepted {wire}"
        );
        assert!(!validator.is_valid(&wire), "schema admitted {wire}");
    }
}

#[test]
fn refusal_categories_have_the_declared_wire_spelling_and_exit_codes() {
    for (category, spelling, code) in [
        (Category::Usage, "usage", 2),
        (Category::InvalidInput, "invalid_input", 65),
        (Category::NotFound, "not_found", 66),
        (Category::Conflict, "conflict", 67),
        (Category::Refused, "refused", 77),
        (Category::Unavailable, "unavailable", 69),
        (Category::Interrupted, "interrupted", 75),
        (Category::Configuration, "configuration", 78),
        (Category::Internal, "internal", 70),
    ] {
        assert_eq!(category.exit_code(), code);
        assert_eq!(serde_json::to_value(category).unwrap(), json!(spelling));
        assert_eq!(
            serde_json::from_value::<Category>(json!(spelling)).unwrap(),
            category
        );
    }
}

#[test]
fn refusals_round_trip_without_changing_contributor_codes() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/refusal.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for code in ["item_conflict", "-32042", "Mixed.Case/é:Code"] {
        let mut refusal = Refusal::new(code, Category::Conflict, "The item has changed.");
        let without_details = serde_json::to_value(&refusal).unwrap();
        assert!(without_details.get("details").is_none());
        validator.validate(&without_details).unwrap();
        refusal.details = Some(json!({ "expected": 1, "actual": 2 }));
        refusal.retryable = true;
        let wire = serde_json::to_value(&refusal).unwrap();
        assert_eq!(
            wire,
            json!({ "code": code, "category": "conflict", "message": "The item has changed.", "details": { "expected": 1, "actual": 2 }, "retryable": true, "evidence": "completed" })
        );
        validator.validate(&wire).unwrap();
        let decoded: Refusal = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(decoded, refusal);
        assert_eq!(decoded.code, code);
        for field in ["code", "category", "message", "retryable", "evidence"] {
            let mut invalid = wire.clone();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<Refusal>(invalid.clone()).is_err());
            assert!(!validator.is_valid(&invalid));
        }
        let mut invalid = wire;
        invalid["category"] = json!("unknown");
        assert!(serde_json::from_value::<Refusal>(invalid.clone()).is_err());
        assert!(!validator.is_valid(&invalid));
    }
}
