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
