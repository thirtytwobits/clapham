//! Reusable differential checks over fresh application contributions.

mod lists;
mod process;

use clapham::{
    Admission, Bounds, CallResult, Cancellation, ComposeOptions, ComposedSet, CompositionCode,
    Contribution, ExecutionMode, Gate, Outcome, Principal, PrincipalKind, RecordSink, Refusal,
    compose_with_options,
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub use lists::{lists_fixture, lists_gate};
pub use process::{Process, ProcessFrontEnd, ProcessKind};

/// One finite invocation against a fresh fixture, including a terminal refusal for a watch.
#[derive(Clone, Debug)]
pub struct Case {
    /// The complete operation name, before any remote binding alias.
    pub operation: String,
    /// Input supplied unchanged through every front end.
    pub input: Value,
    /// The caller authenticated by each host adapter.
    pub principal: Principal,
}
impl Case {
    /// A call by `human:human`; change `principal` to exercise another caller.
    pub fn new(operation: impl Into<String>, input: Value) -> Self {
        Self {
            operation: operation.into(),
            input,
            principal: Principal::new("human", PrincipalKind::Human),
        }
    }
}

/// Reproducible state and explicit inputs; schemas cannot supply meaningful application identities.
#[derive(Clone)]
pub struct Fixture {
    factory: Arc<dyn Fn() -> Vec<Contribution> + Send + Sync>,
    /// Finite cases, with at least one schema-valid input for every contributed operation.
    pub cases: Vec<Case>,
    /// A caller the scripted gate must refuse for every contributed operation, but admit to describe.
    pub denied_principal: Principal,
    /// Bounds used by in-process compositions; external hosts must use the same profile.
    pub bounds: Bounds,
}
impl Fixture {
    /// The factory must recreate the same initial state on every invocation.
    pub fn new(
        factory: impl Fn() -> Vec<Contribution> + Send + Sync + 'static,
        cases: Vec<Case>,
    ) -> Self {
        Self {
            factory: Arc::new(factory),
            cases,
            denied_principal: Principal::new("denied", PrincipalKind::Agent),
            bounds: Bounds::default(),
        }
    }
    /// Fresh handlers and state for one independent probe, also usable by the application binary.
    pub fn contributions(&self) -> Vec<Contribution> {
        (self.factory)()
    }
    fn compose(&self, gate: Arc<dyn Gate>) -> Result<ComposedSet, Failure> {
        compose_with_options(
            self.contributions(),
            self.bounds.clone(),
            ComposeOptions {
                gate,
                execution: ExecutionMode::Daemon,
                ..Default::default()
            },
        )
        .map_err(|error| Failure::new(error.to_string()))
    }
}
impl fmt::Debug for Fixture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fixture")
            .field("cases", &self.cases)
            .field("denied_principal", &self.denied_principal)
            .field("bounds", &self.bounds)
            .finish_non_exhaustive()
    }
}

/// Frames in observed delivery order. Exactly one terminal frame must come last.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A stream record, acknowledged before the next frame.
    Record(Value),
    /// A typed operation outcome.
    Outcome(Outcome),
    /// A native dispatch or operation refusal.
    Refusal(Refusal),
    /// The CLI's final data envelope; compared with the baseline's actual output representation.
    CliResult(Value),
}

/// An actual front-end observation. Keeping frames avoids hiding ordering and duplicate results.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    /// Original operation names returned by describe (before a remote binding's namespace).
    pub catalogue: Vec<String>,
    /// Ordered records followed by exactly one result or refusal.
    pub frames: Vec<Frame>,
    /// Process exit code for CLI observations, absent for other front ends.
    pub exit_code: Option<i32>,
}

/// A failed probe or a conformance mismatch, with front-end and case context in its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure(String);
impl Failure {
    /// Report a driver failure without panicking inside the harness.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
    fn at(self, front_end: &str, operation: &str) -> Self {
        Self(format!("{front_end}, {operation}: {}", self.0))
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Failure {}
impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}
impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// A driver future borrowing its front end and fixture.
pub type ObservationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Observation, Failure>> + Send + 'a>>;

/// A front-end driver. Downstream applications can supply their own process protocol or doubles.
pub trait FrontEnd: Send + Sync {
    /// A unique name in a harness run.
    fn name(&self) -> &str;
    /// Whether this front end carries records (in-process and the CLI in 0.1.0).
    fn carries_records(&self) -> bool;
    /// Whether hidden operations refuse at remote binding selection rather than call admission.
    fn binds_tools(&self) -> bool {
        false
    }
    /// Describe and invoke once, recreating the fixture's state before this probe.
    fn observe<'a>(
        &'a self,
        fixture: &'a Fixture,
        gate: Arc<dyn Gate>,
        case: &'a Case,
    ) -> ObservationFuture<'a>;
}

/// The ordinary composed-set runner, also the harness's independent comparison baseline.
#[derive(Debug, Clone, Copy)]
pub struct InProcess;
impl FrontEnd for InProcess {
    fn name(&self) -> &str {
        "in_process"
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
            let set = fixture.compose(gate)?;
            let catalogue = describe(&set, &case.principal).await?;
            let frames = Arc::new(Mutex::new(Vec::new()));
            let records = {
                let frames = frames.clone();
                RecordSink::new(move |record| {
                    frames.lock().unwrap().push(Frame::Record(record));
                    async { Ok(()) }
                })
            };
            let result = set
                .call(
                    &set.all(),
                    case.principal.clone(),
                    &case.operation,
                    case.input.clone(),
                    records,
                    Cancellation::new(),
                    false,
                )
                .await;
            frames.lock().unwrap().push(terminal(result));
            let frames = frames.lock().unwrap().clone();
            Ok(Observation {
                catalogue,
                frames,
                exit_code: None,
            })
        })
    }
}

fn terminal(result: CallResult) -> Frame {
    match result {
        Ok(outcome) => Frame::Outcome(outcome),
        Err(refusal) => Frame::Refusal(refusal),
    }
}
async fn describe(set: &ComposedSet, principal: &Principal) -> Result<Vec<String>, Failure> {
    let outcome = set
        .call(
            &set.all(),
            principal.clone(),
            "clapham.describe",
            serde_json::json!({}),
            RecordSink::default(),
            Cancellation::new(),
            false,
        )
        .await
        .map_err(|e| Failure::new(e.to_string()))?;
    names(
        outcome
            .result
            .as_ref()
            .ok_or_else(|| Failure::new("describe returned no result"))?,
    )
}
fn names(value: &Value) -> Result<Vec<String>, Failure> {
    value
        .as_array()
        .ok_or_else(|| Failure::new("describe did not return an array"))?
        .iter()
        .map(|record| {
            record["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Failure::new("descriptor has no name"))
        })
        .collect()
}
fn frames(observation: &Observation) -> Result<(&[Frame], &Frame), Failure> {
    let (last, records) = observation
        .frames
        .split_last()
        .ok_or_else(|| Failure::new("no result"))?;
    if matches!(last, Frame::Record(_))
        || records
            .iter()
            .any(|frame| !matches!(frame, Frame::Record(_)))
    {
        return Err(Failure::new(
            "expected records followed by exactly one terminal result",
        ));
    }
    Ok((records, last))
}
fn refusal(frame: &Frame) -> Option<Refusal> {
    match frame {
        Frame::Refusal(refusal) => Some(refusal.clone()),
        Frame::Outcome(outcome) if outcome.is_error => outcome
            .result
            .clone()
            .and_then(|value| serde_json::from_value(value).ok()),
        _ => None,
    }
}
fn compare(
    expected: &Observation,
    actual: &Observation,
    records: bool,
    hidden_at_binding: bool,
) -> Result<(), Failure> {
    let (expected_records, expected_result) = frames(expected)?;
    let (actual_records, actual_result) = frames(actual)?;
    let actual_names = actual.catalogue.iter().collect::<BTreeSet<_>>();
    if actual_names.len() != actual.catalogue.len()
        || actual_names != expected.catalogue.iter().collect::<BTreeSet<_>>()
    {
        return Err(Failure::new(
            "describe catalogue differs from the admitted in-process catalogue",
        ));
    }
    if records && actual_records != expected_records {
        return Err(Failure::new("records differ or arrived out of order"));
    }
    let exit = if let Frame::Refusal(expected) = expected_result {
        let actual = refusal(actual_result).ok_or_else(|| Failure::new("expected a refusal"))?;
        // A hidden remote tool cannot be bound: the client refuses at selection, before dispatch.
        // Its native selection code differs from the server's gate refusal. Ordinary admitted
        // operation refusals are always compared field by field below.
        if hidden_at_binding {
            if actual.code != "mcp_selection"
                || actual.category != clapham::Category::Usage
                || actual.retryable
                || actual.evidence != clapham::Evidence::NotDispatched
            {
                return Err(Failure::new(
                    "hidden remote tool did not refuse at binding selection",
                ));
            }
        } else {
            if actual.code != expected.code {
                return Err(Failure::new("refusal code changed"));
            }
            if actual.category != expected.category {
                return Err(Failure::new("refusal category changed"));
            }
            if actual.retryable != expected.retryable {
                return Err(Failure::new("refusal retryable changed"));
            }
        }
        i32::from(actual.category.exit_code())
    } else {
        let Frame::Outcome(expected) = expected_result else {
            return Err(Failure::new("invalid baseline result"));
        };
        let matches = match actual_result {
            Frame::Outcome(actual) => {
                actual.result == expected.result && actual.is_error == expected.is_error
            }
            Frame::CliResult(actual) => {
                let expected_data = if expected.content.is_none()
                    && !expected.is_error
                    && expected.evidence == clapham::Evidence::Completed
                {
                    expected
                        .result
                        .clone()
                        .unwrap_or(serde_json::to_value(expected)?)
                } else {
                    serde_json::to_value(expected)?
                };
                *actual == expected_data
            }
            _ => false,
        };
        if !matches {
            return Err(Failure::new(
                "result differs from the in-process Outcome.result",
            ));
        }
        0
    };
    if actual.exit_code.is_some_and(|actual| actual != exit) {
        return Err(Failure::new(
            "CLI exit code differs from the refusal category",
        ));
    }
    Ok(())
}

/// Counts of completed checks; success means every selected front end passed every case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Number of caller-supplied invocation cases.
    pub cases: usize,
    /// Number of front ends checked against the in-process baseline.
    pub front_ends: usize,
    /// Ordinary invocation comparisons.
    pub calls: usize,
    /// Denied-call and describe-omission comparisons.
    pub gate_checks: usize,
    /// Namespace collisions refused before any driver starts.
    pub namespace_checks: usize,
}

/// Compare a fresh contribution fixture through all front ends, without timers or retries.
///
/// Each operation needs a schema-valid finite case. Indefinite watches can use a terminal
/// refusal case; finite streams prove record ordering. The scripted gate must admit describe
/// and refuse `fixture.denied_principal` for each contributed operation.
pub async fn run(
    fixture: &Fixture,
    gate: Arc<dyn Gate>,
    front_ends: Vec<Box<dyn FrontEnd>>,
) -> Result<Report, Failure> {
    if front_ends.is_empty() {
        return Err(Failure::new("select at least one front end"));
    }
    let mut driver_names = BTreeSet::new();
    for driver in &front_ends {
        if !driver_names.insert(driver.name()) {
            return Err(Failure::new("front-end names must be unique"));
        }
    }
    let contributions = fixture.contributions();
    let namespaces: Vec<_> = contributions.iter().map(|c| c.namespace.clone()).collect();
    let set = fixture.compose(gate.clone())?;
    for namespace in &namespaces {
        let mut duplicate = fixture.contributions();
        duplicate.push(Contribution::new(namespace));
        let collision = compose_with_options(
            duplicate,
            fixture.bounds.clone(),
            ComposeOptions {
                gate: gate.clone(),
                ..Default::default()
            },
        );
        let error = match collision {
            Ok(_) => return Err(Failure::new("duplicate namespace was accepted")),
            Err(error) => error,
        };
        if error.code != CompositionCode::NamespaceClaimed {
            return Err(Failure::new(format!(
                "duplicate namespace refused as {}",
                error.code.as_str()
            )));
        }
    }
    if fixture.cases.is_empty() || namespaces.is_empty() {
        return Err(Failure::new(
            "supply a contribution and finite invocation cases",
        ));
    }
    let mut probes = Vec::new();
    for operation in set.operations().filter(|op| {
        namespaces
            .iter()
            .any(|ns| op.name.namespace() == Some(ns.as_str()))
    }) {
        let case = fixture
            .cases
            .iter()
            .find(|case| {
                case.operation == operation.name.as_str()
                    && set
                        .get(&case.operation)
                        .unwrap()
                        .validate_input(&case.input)
                        .is_ok()
            })
            .ok_or_else(|| Failure::new(format!("{} has no schema-valid case", operation.name)))?;
        if gate.admit(&fixture.denied_principal, operation) == Admission::Allow {
            return Err(Failure::new(format!(
                "scripted gate does not refuse {}",
                operation.name
            )));
        }
        let mut denied = case.clone();
        denied.principal = fixture.denied_principal.clone();
        probes.push(denied);
    }
    for case in &fixture.cases {
        if !set.get(&case.operation).is_some_and(|operation| {
            namespaces.iter().any(|namespace| {
                operation.descriptor().name.namespace() == Some(namespace.as_str())
            })
        }) {
            return Err(Failure::new(format!(
                "case operation is not contributed by the fixture: {}",
                case.operation
            )));
        }
    }
    let mut report = Report {
        cases: fixture.cases.len(),
        front_ends: front_ends.len(),
        calls: 0,
        gate_checks: 0,
        namespace_checks: namespaces.len(),
    };
    for (cases, gated) in [(&fixture.cases, false), (&probes, true)] {
        for case in cases {
            let baseline = InProcess.observe(fixture, gate.clone(), case).await?;
            if gated
                && (baseline.catalogue.contains(&case.operation)
                    || refusal(frames(&baseline)?.1).is_none())
            {
                return Err(Failure::new(
                    "scripted gate did not hide and refuse the operation",
                ));
            }
            for driver in &front_ends {
                let observation = driver
                    .observe(fixture, gate.clone(), case)
                    .await
                    .map_err(|e| e.at(driver.name(), &case.operation))?;
                compare(
                    &baseline,
                    &observation,
                    driver.carries_records(),
                    driver.binds_tools()
                        && !baseline.catalogue.contains(&case.operation)
                        && refusal(frames(&baseline)?.1).is_some_and(|r| {
                            matches!(r.code.as_str(), "not_admitted" | "admission_pending")
                        }),
                )
                .map_err(|e| e.at(driver.name(), &case.operation))?;
                if gated {
                    report.gate_checks += 1;
                } else {
                    report.calls += 1;
                }
            }
        }
    }
    Ok(report)
}
