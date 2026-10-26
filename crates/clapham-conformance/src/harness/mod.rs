//! Reusable differential checks over fresh application contributions.

mod lists;
mod process;

use clapham::{
    Admission, Bounds, Call, CallResult, Cancellation, ComposeOptions, ComposedSet,
    CompositionCode, CompositionError, Configuration, Contribution, ExecutionMode, Exposure, Gate,
    OAuthContext, Outcome, RecordSink, Refusal, compose_with_options,
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
#[derive(Clone)]
pub struct Case {
    /// The complete operation name, before any remote binding alias.
    pub operation: String,
    /// Input supplied unchanged through every front end.
    pub input: Value,
    /// The caller authenticated by each host adapter, preserving the verified subject and grant.
    pub auth: OAuthContext,
    projection: Option<Projection>,
}
type Projection = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
impl Case {
    /// A call by `human:human`; change `auth` to exercise another caller.
    pub fn new(operation: impl Into<String>, input: Value) -> Self {
        Self {
            operation: operation.into(),
            input,
            auth: oauth_context(
                "human",
                &["lists:read", "lists:write", "lists:admin", "items:tick"],
            ),
            projection: None,
        }
    }
    /// Compare `projection` of a successful result rather than the whole result. The harness
    /// applies it to the baseline's and each front end's result alike. Project out only what the
    /// operation mints, such as an identifier, revision or timestamp.
    pub fn compare(mut self, projection: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        self.projection = Some(Arc::new(projection));
        self
    }
    fn project(&self, result: &Value) -> Value {
        match &self.projection {
            Some(projection) => projection(result),
            None => result.clone(),
        }
    }
}
impl fmt::Debug for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Case")
            .field("operation", &self.operation)
            .field("input", &self.input)
            .field("auth", &self.auth)
            .field("projected", &self.projection.is_some())
            .finish()
    }
}

/// Reproducible state and explicit inputs; schemas cannot supply meaningful application identities.
#[derive(Clone)]
pub struct Fixture {
    composition: Composition,
    /// Finite cases, with at least one schema-valid input for every composed operation.
    pub cases: Vec<Case>,
    /// A caller the scripted gate must refuse for every composed operation, but admit to describe.
    pub denied_auth: OAuthContext,
    /// Bounds used by every composition of the fixture; external hosts must use the same profile.
    pub bounds: Bounds,
}
#[derive(Clone)]
enum Composition {
    Contributions(Arc<dyn Fn() -> Vec<Contribution> + Send + Sync>),
    Configuration(Arc<dyn Fn() -> Configuration + Send + Sync>),
}
/// A source and provider name no fixture configuration may use; it claims a fixture namespace.
const COLLISION_SOURCE: &str = "clapham.conformance.collision";
impl Fixture {
    /// Compose contributions directly: every operation is exposed, no resource policy is
    /// registered and no OAuth resource is bound. The factory must recreate the same initial
    /// state on every invocation.
    pub fn new(
        factory: impl Fn() -> Vec<Contribution> + Send + Sync + 'static,
        cases: Vec<Case>,
    ) -> Self {
        Self::with(Composition::Contributions(Arc::new(factory)), cases)
    }
    /// Compose a configuration with its providers, resource policies, exposures and OAuth
    /// resource binding. The factory must recreate the same initial state on every invocation.
    pub fn configured(
        factory: impl Fn() -> Configuration + Send + Sync + 'static,
        cases: Vec<Case>,
    ) -> Self {
        Self::with(Composition::Configuration(Arc::new(factory)), cases)
    }
    fn with(composition: Composition, cases: Vec<Case>) -> Self {
        Self {
            composition,
            cases,
            denied_auth: oauth_context("denied", &["lists:read", "lists:write"]),
            bounds: Bounds::default(),
        }
    }
    /// Fresh handlers and state for one independent probe, composed within the fixture's bounds.
    /// The fixture's host executable composes through this method with its own options.
    pub fn compose(&self, options: ComposeOptions) -> Result<ComposedSet, CompositionError> {
        match &self.composition {
            Composition::Contributions(factory) => {
                compose_with_options(factory(), self.bounds.clone(), options)
            }
            Composition::Configuration(factory) => {
                factory().compose_with_options(self.bounds.clone(), options)
            }
        }
    }
    fn baseline(&self, gate: Arc<dyn Gate>) -> Result<ComposedSet, Failure> {
        self.compose(daemon(gate))
            .map_err(|error| Failure::new(error.to_string()))
    }
    /// Namespaces the fixture claims: those of its contributions, or of its exposed operations.
    fn namespaces(&self, set: &ComposedSet) -> Vec<String> {
        match &self.composition {
            Composition::Contributions(factory) => factory()
                .into_iter()
                .map(|contribution| contribution.namespace)
                .collect(),
            Composition::Configuration(_) => set
                .operations()
                .filter_map(|operation| operation.name.namespace())
                .filter(|namespace| *namespace != "clapham")
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }
    /// Compose the fixture with another registration claiming `namespace`, and check the refusal.
    fn check_collision(
        &self,
        set: &ComposedSet,
        namespace: &str,
        gate: Arc<dyn Gate>,
    ) -> Result<(), Failure> {
        let (collision, expected) = match &self.composition {
            Composition::Contributions(factory) => {
                let mut duplicate = factory();
                duplicate.push(Contribution::new(namespace));
                (
                    compose_with_options(duplicate, self.bounds.clone(), daemon(gate)),
                    CompositionCode::NamespaceClaimed,
                )
            }
            Composition::Configuration(factory) => {
                // A configuration refuses a second source exposing an operation in a claimed
                // namespace. The handler is never dispatched.
                let operation = set
                    .operations()
                    .find(|operation| operation.name.namespace() == Some(namespace))
                    .ok_or_else(|| Failure::new(format!("{namespace} exposes no operation")))?
                    .clone();
                let name = operation.name.to_string();
                let claim = Contribution::new(namespace).operation(operation, |_: Call| async {
                    Err(Refusal::not_dispatched(
                        "unreachable",
                        clapham::Category::Internal,
                        "the collision probe is never composed",
                    ))
                });
                (
                    factory()
                        .provider(COLLISION_SOURCE, claim)
                        .expose(Exposure::new(COLLISION_SOURCE, name))
                        .compose_with_options(self.bounds.clone(), daemon(gate)),
                    CompositionCode::InvalidConfiguration,
                )
            }
        };
        let error = match collision {
            Ok(_) => return Err(Failure::new("duplicate namespace was accepted")),
            Err(error) => error,
        };
        if error.code != expected {
            return Err(Failure::new(format!(
                "duplicate namespace refused as {}",
                error.code.as_str()
            )));
        }
        Ok(())
    }
}
fn daemon(gate: Arc<dyn Gate>) -> ComposeOptions {
    ComposeOptions {
        gate,
        execution: ExecutionMode::Daemon,
        ..Default::default()
    }
}
impl fmt::Debug for Fixture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fixture")
            .field("cases", &self.cases)
            .field("denied_auth", &self.denied_auth)
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
    /// Whether this front end carries records.
    fn carries_records(&self) -> bool;
    /// Whether hidden operations refuse at remote binding selection rather than call admission.
    fn binds_tools(&self) -> bool {
        false
    }
    /// Native refusal when discovery hides a tool before local binding or parsing.
    fn hidden_refusal_code(&self) -> Option<&str> {
        if self.binds_tools() {
            Some("mcp_selection")
        } else {
            None
        }
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
            let set = fixture.baseline(gate)?;
            let catalogue = describe(&set, &case.auth).await?;
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
                    case.auth.clone(),
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
async fn describe(set: &ComposedSet, auth: &OAuthContext) -> Result<Vec<String>, Failure> {
    let outcome = set
        .call(
            &set.all(),
            auth.clone(),
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
/// Whether clapham-cli emits an outcome's result alone rather than the complete outcome.
fn cli_result_is_bare(outcome: &Outcome) -> bool {
    outcome.content.is_none()
        && !outcome.is_error
        && outcome.evidence == clapham::Evidence::Completed
}
fn compare(
    case: &Case,
    expected: &Observation,
    actual: &Observation,
    records: bool,
    hidden_at_binding: Option<&str>,
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
        if let Some(code) = hidden_at_binding {
            if actual.code != code
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
            if actual.evidence != expected.evidence {
                return Err(Failure::new("refusal evidence changed"));
            }
        }
        i32::from(actual.category.exit_code())
    } else {
        let Frame::Outcome(baseline) = expected_result else {
            return Err(Failure::new("invalid baseline result"));
        };
        // A case's projection applies to successful results; a tool error compares whole.
        let project = |result: &Value| {
            if baseline.is_error {
                result.clone()
            } else {
                case.project(result)
            }
        };
        let mut expected = baseline.clone();
        expected.result = baseline.result.as_ref().map(project);
        let matches = match actual_result {
            Frame::Outcome(actual) => {
                actual.result.as_ref().map(project) == expected.result
                    && actual.is_error == expected.is_error
            }
            // Mirror clapham-cli's output::result_data representation.
            Frame::CliResult(actual) => match &expected.result {
                Some(result) if cli_result_is_bare(&expected) => project(actual) == *result,
                _ => {
                    let mut actual = actual.clone();
                    if let Some(result) = actual.get_mut("result") {
                        *result = project(result);
                    }
                    actual == serde_json::to_value(&expected)?
                }
            },
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

/// Compare a fresh fixture through all front ends, without timers or retries.
///
/// Each operation needs a schema-valid finite case. Indefinite watches can use a terminal
/// refusal case; finite streams prove record ordering. The scripted gate must admit describe
/// and refuse `fixture.denied_auth` for each composed operation.
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
    let set = fixture.baseline(gate.clone())?;
    let namespaces = fixture.namespaces(&set);
    for namespace in &namespaces {
        fixture.check_collision(&set, namespace, gate.clone())?;
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
        if gate.admit(&fixture.denied_auth, operation) == Admission::Allow {
            return Err(Failure::new(format!(
                "scripted gate does not refuse {}",
                operation.name
            )));
        }
        let mut denied = case.clone();
        denied.auth = fixture.denied_auth.clone();
        probes.push(denied);
    }
    for case in &fixture.cases {
        if !set.get(&case.operation).is_some_and(|operation| {
            namespaces.iter().any(|namespace| {
                operation.descriptor().name.namespace() == Some(namespace.as_str())
            })
        }) {
            return Err(Failure::new(format!(
                "case operation is not composed by the fixture: {}",
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
                    case,
                    &baseline,
                    &observation,
                    driver.carries_records(),
                    driver.hidden_refusal_code().filter(|_| {
                        !baseline.catalogue.contains(&case.operation)
                            && refusal(frames(&baseline).expect("baseline frames").1).is_some_and(
                                |r| {
                                    matches!(
                                        r.code.as_str(),
                                        "not_admitted" | "admission_pending" | "insufficient_scope"
                                    )
                                },
                            )
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

fn oauth_context(subject: impl Into<String>, scopes: &[&str]) -> clapham::OAuthContext {
    clapham::OAuthContext::from_verified(
        "https://identity.example.com",
        "https://lists.example.com",
        subject,
        scopes.iter().copied(),
    )
}
