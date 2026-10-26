//! Adversarial front-end doubles demonstrate that the harness rejects contract loss.

use clapham::Gate;
use clapham_conformance::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Double {
    change: fn(&mut Observation),
    entered: Arc<AtomicUsize>,
}
impl FrontEnd for Double {
    fn name(&self) -> &str {
        "double"
    }
    fn carries_records(&self) -> bool {
        true
    }
    fn observe<'a>(
        &'a self,
        fixture: &'a Fixture,
        gate: Arc<dyn Gate>,
        case: &'a Case,
    ) -> ObservationFuture<'a> {
        Box::pin(async move {
            self.entered.fetch_add(1, Ordering::SeqCst);
            let mut observation = InProcess.observe(fixture, gate, case).await?;
            (self.change)(&mut observation);
            Ok(observation)
        })
    }
}
async fn rejects(change: fn(&mut Observation), diagnostic: &str) {
    let fixture = lists_fixture();
    let double = Double {
        change,
        entered: Arc::new(AtomicUsize::new(0)),
    };
    let failure = run(&fixture, lists_gate(), vec![Box::new(double)])
        .await
        .unwrap_err();
    assert!(failure.to_string().contains(diagnostic), "{failure}");
}
#[tokio::test]
async fn dropping_retryable_fails_the_harness() {
    rejects(
        |o| {
            for frame in &mut o.frames {
                if let Frame::Refusal(r) = frame {
                    r.retryable = false;
                }
            }
        },
        "retryable",
    )
    .await;
}
#[tokio::test]
async fn changed_refusal_codes_and_categories_fail() {
    rejects(
        |o| {
            for frame in &mut o.frames {
                if let Frame::Refusal(r) = frame {
                    r.code = "changed".into();
                }
            }
        },
        "code",
    )
    .await;
    rejects(
        |o| {
            for frame in &mut o.frames {
                if let Frame::Refusal(r) = frame {
                    r.category = clapham::Category::Usage;
                }
            }
        },
        "category",
    )
    .await;
}
#[tokio::test]
async fn changed_refusal_evidence_fails() {
    rejects(
        |o| {
            for frame in &mut o.frames {
                if let Frame::Refusal(r) = frame {
                    r.evidence = clapham::Evidence::Uncertain;
                }
            }
        },
        "evidence",
    )
    .await;
}
#[tokio::test]
async fn resource_policies_attached_by_exposure_and_descriptor_refuse_in_the_baseline() {
    let fixture = lists_fixture();
    for (operation, list, code, category) in [
        (
            "lists.get",
            serde_json::json!({"id":"private"}),
            "list_not_found",
            clapham::Category::NotFound,
        ),
        (
            "items.tick",
            serde_json::json!({"list_id":"private","id":"p"}),
            "list_access_denied",
            clapham::Category::Refused,
        ),
    ] {
        let cases: Vec<_> = fixture
            .cases
            .iter()
            .filter(|case| case.operation == operation && case.input == list)
            .collect();
        let [refused, owned] = cases.as_slice() else {
            panic!("expected a refused and an owned {operation} case");
        };
        let observed = InProcess
            .observe(&fixture, lists_gate(), refused)
            .await
            .unwrap();
        let [Frame::Refusal(refusal)] = observed.frames.as_slice() else {
            panic!("expected {operation} to refuse: {:?}", observed.frames);
        };
        assert_eq!(
            (refusal.code.as_str(), refusal.category, refusal.evidence),
            (code, category, clapham::Evidence::NotDispatched)
        );
        let observed = InProcess
            .observe(&fixture, lists_gate(), owned)
            .await
            .unwrap();
        let [Frame::Outcome(outcome)] = observed.frames.as_slice() else {
            panic!("expected {operation} to complete: {:?}", observed.frames);
        };
        assert!(!outcome.is_error);
    }
}
#[tokio::test]
async fn wrong_records_early_results_and_duplicate_results_fail() {
    rejects(
        |o| {
            if o.frames.len() > 2 {
                o.frames.swap(0, 1);
            }
        },
        "records",
    )
    .await;
    rejects(
        |o| {
            if o.frames.len() > 2 {
                o.frames.reverse();
            }
        },
        "terminal",
    )
    .await;
    rejects(
        |o| o.frames.push(o.frames.last().unwrap().clone()),
        "terminal",
    )
    .await;
}
#[tokio::test]
async fn wrong_exit_code_and_gate_visibility_fail() {
    rejects(|o| o.exit_code = Some(0), "exit code").await;
    rejects(
        |o| o.catalogue.push("unadmitted.operation".into()),
        "catalogue",
    )
    .await;
}
#[tokio::test]
async fn namespace_collision_and_missing_cases_fail_before_a_driver_starts() {
    let entered = Arc::new(AtomicUsize::new(0));
    let driver = || {
        Box::new(Double {
            change: |_| {},
            entered: entered.clone(),
        }) as Box<dyn FrontEnd>
    };
    let fixture = Fixture::new(
        || {
            vec![
                clapham::Contribution::new("lists"),
                clapham::Contribution::new("lists"),
            ]
        },
        vec![],
    );
    assert!(
        run(&fixture, lists_gate(), vec![driver()])
            .await
            .unwrap_err()
            .to_string()
            .contains("namespace_claimed")
    );
    assert_eq!(entered.load(Ordering::SeqCst), 0);
    let mut fixture = lists_fixture();
    fixture.cases.retain(|case| case.operation != "items.tick");
    assert!(
        run(&fixture, lists_gate(), vec![driver()])
            .await
            .unwrap_err()
            .to_string()
            .contains("schema-valid case")
    );
    assert_eq!(entered.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn refusal_shaped_tool_errors_remain_successful_outcomes() {
    use clapham::{Call, Category, Contribution, Operation, Outcome, Refusal};
    use serde_json::json;
    let fixture = Fixture::new(
        || {
            let descriptor: Operation = serde_json::from_value(json!({
                "name":"items.failure", "description":"Return a tool error as structured content.",
                "input":{"type":"object"},"effect":"query","locality":"workspace",
                "idempotent":true,"open_world":false
            }))
            .unwrap();
            vec![
                Contribution::new("items").operation(descriptor, |_: Call| async {
                    let refusal =
                        Refusal::new("tool_failure", Category::Conflict, "Tool-reported error");
                    let mut outcome = Outcome::new(serde_json::to_value(refusal).unwrap());
                    outcome.is_error = true;
                    Ok(outcome)
                }),
            ]
        },
        vec![Case::new("items.failure", json!({}))],
    );
    // Model the CLI's complete outcome envelope without treating its structured refusal
    // as a native dispatch refusal. It still exits successfully for a tool-error outcome.
    let driver = Double {
        entered: Arc::new(AtomicUsize::new(0)),
        change: |o| {
            o.exit_code = Some(match o.frames.last().unwrap() {
                Frame::Refusal(refusal) => i32::from(refusal.category.exit_code()),
                _ => 0,
            });
            for frame in &mut o.frames {
                if let Frame::Outcome(outcome) = frame {
                    *frame = Frame::CliResult(serde_json::to_value(outcome).unwrap());
                }
            }
        },
    };
    run(&fixture, lists_gate(), vec![Box::new(driver)])
        .await
        .unwrap();
}

/// Each fresh composition mints the next identifier, as an operation minting state would.
fn minting(content: bool, projected: bool) -> Fixture {
    use clapham::{Call, Contribution, Operation, Outcome};
    use serde_json::json;
    let minted = Arc::new(AtomicUsize::new(0));
    let mut case = Case::new("items.mint", json!({}));
    if projected {
        case = case.compare(|result| {
            let mut result = result.clone();
            result.as_object_mut().unwrap().remove("id");
            result
        });
    }
    Fixture::new(
        move || {
            let id = minted.fetch_add(1, Ordering::SeqCst);
            let descriptor: Operation = serde_json::from_value(json!({
                "name":"items.mint", "description":"Return a minted identifier.",
                "input":{"type":"object"},"effect":"query","locality":"workspace",
                "idempotent":true,"open_world":false
            }))
            .unwrap();
            vec![
                Contribution::new("items").operation(descriptor, move |_: Call| async move {
                    let mut outcome = Outcome::new(json!({"id":id,"name":"New"}));
                    if content {
                        outcome.content = Some(vec![json!({"type":"text","text":"New"})]);
                    }
                    Ok(outcome)
                }),
            ]
        },
        vec![case],
    )
}
fn rename(o: &mut Observation) {
    for frame in &mut o.frames {
        if let Frame::Outcome(outcome) = frame {
            outcome.result.as_mut().unwrap()["name"] = "Changed".into();
        }
    }
}
/// Model clapham-cli's data: a completed ordinary result alone, otherwise the whole outcome.
fn as_cli(o: &mut Observation) {
    o.exit_code = Some(match o.frames.last().unwrap() {
        Frame::Refusal(refusal) => i32::from(refusal.category.exit_code()),
        _ => 0,
    });
    for frame in &mut o.frames {
        if let Frame::Outcome(outcome) = frame {
            *frame = Frame::CliResult(match &outcome.result {
                Some(result) if outcome.content.is_none() => result.clone(),
                _ => serde_json::to_value(&*outcome).unwrap(),
            });
        }
    }
}
async fn minted(fixture: &Fixture, change: fn(&mut Observation)) -> Result<Report, Failure> {
    let driver = Double {
        change,
        entered: Arc::new(AtomicUsize::new(0)),
    };
    run(fixture, lists_gate(), vec![Box::new(driver)]).await
}
#[tokio::test]
async fn minted_results_differ_without_a_projection() {
    let failure = minted(&minting(false, false), |_| {}).await.unwrap_err();
    assert!(failure.to_string().contains("result differs"), "{failure}");
}
#[tokio::test]
async fn projected_minted_results_pass() {
    minted(&minting(false, true), |_| {}).await.unwrap();
}
#[tokio::test]
async fn projected_results_still_compare_what_they_keep() {
    let failure = minted(&minting(false, true), rename).await.unwrap_err();
    assert!(failure.to_string().contains("result differs"), "{failure}");
}
#[tokio::test]
async fn projections_apply_to_cli_data() {
    for content in [false, true] {
        minted(&minting(content, true), as_cli).await.unwrap();
        let failure = minted(&minting(content, true), |o| {
            rename(o);
            as_cli(o);
        })
        .await
        .unwrap_err();
        assert!(failure.to_string().contains("result differs"), "{failure}");
        assert!(minted(&minting(content, false), as_cli).await.is_err());
    }
}
