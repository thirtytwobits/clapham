//! Shared limits, dispatch evidence and lossless ordered content.

use clapham::{
    Bounds, Category, ContentBlock, Evidence, Outcome, Refusal, ResourceContents, compose,
};
use serde_json::{Value, json};

#[test]
fn default_bounds_match_the_connection_budgets_and_validate() {
    let bounds = Bounds::default();
    assert_eq!(
        (
            bounds.message_bytes,
            bounds.catalogue_bytes,
            bounds.schema_bytes
        ),
        (1024 * 1024, 8 * 1024 * 1024, 128 * 1024)
    );
    assert_eq!(
        (
            bounds.tools,
            bounds.pages,
            bounds.in_flight,
            bounds.waiting,
            bounds.notifications
        ),
        (512, 32, 4, 16, 4096)
    );
    bounds.validate().unwrap();
    assert_eq!(compose([], bounds).unwrap().operations().len(), 0);
}

#[test]
fn minimum_catalogue_bound_fits_the_empty_json_array() {
    let invalid = Bounds {
        catalogue_bytes: 1,
        ..Bounds::default()
    };
    assert!(invalid.validate().is_err());
    assert_eq!(
        compose([], invalid).unwrap_err().code.as_str(),
        "invalid_bounds"
    );
    let minimum = Bounds {
        catalogue_bytes: 2,
        ..Bounds::default()
    };
    minimum.validate().unwrap();
    assert_eq!(compose([], minimum).unwrap().operations().len(), 0);
}

#[test]
fn pre_dispatch_constructor_carries_explicit_evidence_in_the_wire_form() {
    let refusal = Refusal::not_dispatched("list_missing", Category::NotFound, "List not found.");
    assert_eq!(refusal.evidence, Evidence::NotDispatched);
    assert_eq!(
        serde_json::to_value(&refusal).unwrap()["evidence"],
        "not_dispatched"
    );
    assert_eq!(refusal.code, "list_missing");
    assert!(!refusal.retryable);
}

#[test]
fn invalid_bounds_are_refused_before_composition_including_overflow_and_total_buffering() {
    for field in 0..8 {
        for value in [0, usize::MAX] {
            let mut bounds = Bounds::default();
            match field {
                0 => bounds.message_bytes = value,
                1 => bounds.catalogue_bytes = value,
                2 => bounds.schema_bytes = value,
                3 => bounds.tools = value,
                4 => bounds.pages = value,
                5 => bounds.in_flight = value,
                6 => bounds.waiting = value,
                _ => bounds.notifications = value,
            }
            if field == 6 && value == 0 {
                bounds.validate().unwrap();
                continue;
            }
            assert!(
                bounds.validate().is_err(),
                "accepted field {field} = {value}"
            );
            assert_eq!(
                compose([], bounds).unwrap_err().code.as_str(),
                "invalid_bounds"
            );
        }
    }
    let mut bounds = Bounds {
        message_bytes: 8 * 1024 * 1024,
        in_flight: 1,
        waiting: 5,
        ..Bounds::default()
    };
    bounds.validate().unwrap();
    bounds.waiting += 1;
    assert!(bounds.validate().is_err());
    bounds = Bounds::default();
    bounds.schema_bytes = bounds.message_bytes + 1;
    assert!(bounds.validate().is_err());
    bounds = Bounds::default();
    bounds.message_bytes = 8191;
    assert!(bounds.validate().is_err());
}

#[test]
fn ordered_content_keeps_all_block_kinds_and_binary_payloads_without_fetching() {
    let content = vec![
        ContentBlock::Text {
            text: "first".into(),
        },
        ContentBlock::Image {
            url: "data:image/png;base64,AA==".into(),
            mime_type: "image/png".into(),
        },
        ContentBlock::Audio {
            url: "data:audio/wav;base64,AA==".into(),
            mime_type: "audio/wav".into(),
        },
        ContentBlock::ResourceLink {
            uri: "lists://list-1".into(),
            name: "List".into(),
            mime_type: None,
        },
        ContentBlock::Resource {
            uri: "items://item-1".into(),
            mime_type: Some("text/plain".into()),
            contents: ResourceContents::Text("embedded".into()),
        },
        ContentBlock::Resource {
            uri: "items://item-2".into(),
            mime_type: None,
            contents: ResourceContents::Base64("AA==".into()),
        },
        ContentBlock::Text {
            text: "last".into(),
        },
    ];
    let mut outcome = Outcome::new(json!({ "code": "list_missing" }));
    outcome.content = Some(content.clone());
    outcome.is_error = true;
    let wire = serde_json::to_value(&outcome).unwrap();
    let restored: Outcome = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(restored, outcome);
    assert_eq!(restored.content, Some(content));
    assert_eq!(
        wire["content"][0],
        json!({ "type": "text", "text": "first" })
    );
    assert_eq!(
        wire["content"][5]["contents"],
        json!({ "encoding": "base64", "data": "AA==" })
    );
    assert_eq!(
        wire["content"][6],
        json!({ "type": "text", "text": "last" })
    );
}

#[test]
fn absent_and_null_structured_results_and_absent_and_empty_content_remain_distinct() {
    for result in [None, Some(Value::Null)] {
        for content in [None, Some(Vec::new())] {
            let mut outcome = Outcome::new(Value::Null);
            outcome.result = result.clone();
            outcome.content = content;
            let wire = serde_json::to_value(&outcome).unwrap();
            assert_eq!(wire.get("result").is_some(), result.is_some());
            assert_eq!(serde_json::from_value::<Outcome>(wire).unwrap(), outcome);
        }
    }
}

#[test]
fn every_refusal_evidence_state_round_trips_and_validates_against_the_published_schema() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/refusal.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (evidence, spelling) in [
        (Evidence::NotDispatched, "not_dispatched"),
        (Evidence::Completed, "completed"),
        (Evidence::Uncertain, "uncertain"),
    ] {
        let refusal = Refusal::new("list_missing", Category::NotFound, "List not found.")
            .with_evidence(evidence);
        let wire = serde_json::to_value(&refusal).unwrap();
        assert_eq!(wire["evidence"], spelling);
        validator.validate(&wire).unwrap();
        assert_eq!(serde_json::from_value::<Refusal>(wire).unwrap(), refusal);
    }
}
