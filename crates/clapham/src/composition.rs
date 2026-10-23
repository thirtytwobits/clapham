use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Write};
use std::sync::Arc;

use futures_util::future::{Either, select};
use serde::Serialize;
use serde_json::Value;

use crate::builtins::Dispatch;

use crate::{
    Bounds, Call, Category, ComposeOptions, Evidence, ExecutionMode, Gate, Handler, Operation,
    Outcome, Refusal, Selection,
};

/// One namespace's descriptors, each paired with an asynchronous handler.
pub struct Contribution {
    /// The namespace owned by this contribution.
    pub namespace: String,
    /// Descriptors and their handlers, in contribution order.
    pub operations: Vec<(Operation, Box<dyn Handler>)>,
    /// Opaque revision of a remote binding, included in selections that use it.
    pub binding_revision: Option<String>,
}

impl Contribution {
    /// An empty contribution claiming `namespace`.
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            operations: Vec::new(),
            binding_revision: None,
        }
    }

    /// Record the remote binding revision without changing operation descriptors.
    pub fn with_binding_revision(mut self, revision: impl Into<String>) -> Self {
        self.binding_revision = Some(revision.into());
        self
    }

    /// Add an operation, accepting a handler object or an asynchronous closure.
    /// Use [`Self::operation_boxed`] for a handler that is already boxed.
    pub fn operation(self, descriptor: Operation, handler: impl Handler + 'static) -> Self {
        self.operation_boxed(descriptor, Box::new(handler))
    }

    /// Add an already boxed handler without allocating another box around it.
    pub fn operation_boxed(mut self, descriptor: Operation, handler: Box<dyn Handler>) -> Self {
        self.operations.push((descriptor, handler));
        self
    }
}

impl fmt::Debug for Contribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Contribution")
            .field("namespace", &self.namespace)
            .field("operations", &self.operations.len())
            .finish()
    }
}

/// A composition failure's stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionCode {
    /// A namespace is repeated or reserved for the built-in contribution.
    NamespaceClaimed,
    /// A namespace or operation name is invalid, or the operation belongs elsewhere.
    InvalidName,
    /// An operation's description is empty or whitespace.
    MissingDescription,
    /// A schema is invalid, cannot resolve locally, or the input root is not an object.
    InvalidSchema,
    /// An input or output schema exceeds the byte bound.
    SchemaTooLarge,
    /// An operation's complete name is repeated.
    DuplicateOperation,
    /// The shared bounds are invalid.
    InvalidBounds,
    /// The serialised descriptor catalogue exceeds its byte bound.
    CatalogueTooLarge,
    /// The operation count exceeds the catalogue's limit.
    TooManyOperations,
}

impl CompositionCode {
    /// The stable snake-case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NamespaceClaimed => "namespace_claimed",
            Self::InvalidName => "invalid_name",
            Self::MissingDescription => "missing_description",
            Self::InvalidSchema => "invalid_schema",
            Self::SchemaTooLarge => "schema_too_large",
            Self::DuplicateOperation => "duplicate_operation",
            Self::InvalidBounds => "invalid_bounds",
            Self::CatalogueTooLarge => "catalogue_too_large",
            Self::TooManyOperations => "too_many_operations",
        }
    }
}

/// A descriptor or contribution that prevented composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionError {
    /// The stable failure code.
    pub code: CompositionCode,
    /// A description of the invalid contribution, operation or bound.
    pub message: String,
}

impl CompositionError {
    fn new(code: CompositionCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CompositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for CompositionError {}

/// A descriptor, its compiled validators and its handler, fixed at composition.
pub struct ComposedOperation {
    descriptor: Arc<Operation>,
    input: jsonschema::Validator,
    output: Option<jsonschema::Validator>,
    handler: Dispatch,
    pub(crate) binding_revision: Option<Arc<str>>,
}

impl fmt::Debug for ComposedOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ComposedOperation")
            .field("name", &self.descriptor.name)
            .finish_non_exhaustive()
    }
}

impl ComposedOperation {
    /// The immutable descriptor.
    pub fn descriptor(&self) -> &Operation {
        &self.descriptor
    }

    /// Validate arguments with the schema compiled at composition.
    pub fn validate_input(&self, input: &Value) -> Result<(), Refusal> {
        self.input.validate(input).map_err(|error| {
            Refusal::not_dispatched("invalid_input", Category::InvalidInput, error.to_string())
        })
    }

    /// Validate a successful result with the compiled output schema, if declared.
    /// Tool-reported errors carry their own result shape and are not successful output.
    pub fn validate_output(&self, outcome: &Outcome) -> Result<(), Refusal> {
        if outcome.is_error {
            return Ok(());
        }
        if let Some(validator) = &self.output {
            let result = outcome.result.as_ref().ok_or_else(|| {
                Refusal::new(
                    "invalid_result",
                    Category::Internal,
                    "the operation returned no structured result",
                )
                .with_evidence(outcome.evidence)
            })?;
            validator.validate(result).map_err(|error| {
                Refusal::new("invalid_result", Category::Internal, error.to_string())
                    .with_evidence(outcome.evidence)
            })?;
        }
        Ok(())
    }

    /// Dispatch with validated input under this operation's own name, then validate output.
    ///
    /// This is a low-level dispatch primitive: the host applies selection, admission,
    /// preview and declared-refusal policies around it. Cancellation before dispatch
    /// is certain; cancellation once the handler is entered is uncertain. The handler's
    /// explicit evidence is preserved when it returns, including remote reply loss.
    /// If cancellation and completion are both ready when polled, cancellation wins
    /// and the result is conservatively reported as uncertain.
    /// For `clapham.describe`, this primitive considers the full catalogue and filters
    /// it through the gate; use [`ComposedSet::call`] to apply a caller's selection.
    pub async fn invoke(&self, call: Call) -> Result<Outcome, Refusal> {
        if call.cancel.is_cancelled() {
            return Err(cancelled(Evidence::NotDispatched));
        }
        self.validate_input(&call.input)?;
        let outcome = self.dispatch_validated(call, None).await??;
        self.validate_output(&outcome)?;
        Ok(outcome)
    }

    // Keep handler refusals separate from dispatcher-generated cancellation so
    // the public call runner can enforce the declared refusal contract.
    pub(crate) async fn dispatch_validated(
        &self,
        mut call: Call,
        selection: Option<&Selection>,
    ) -> Result<Result<Outcome, Refusal>, Refusal> {
        if call.cancel.is_cancelled() {
            return Err(cancelled(Evidence::NotDispatched));
        }
        call.name = self.descriptor.name.clone();
        let cancel = call.cancel.clone();
        match select(
            Box::pin(cancel.cancelled()),
            self.handler.call(call, selection),
        )
        .await
        {
            Either::Left(_) => Err(cancelled(Evidence::Uncertain)),
            Either::Right((outcome, _)) => Ok(outcome),
        }
    }
}

pub(crate) fn cancelled(evidence: Evidence) -> Refusal {
    let mut refusal = Refusal::new("cancelled", Category::Interrupted, "the call was cancelled")
        .with_evidence(evidence);
    refusal.retryable = true;
    refusal
}

/// An immutable, cheaply clonable set of operations and their compiled schemas.
#[derive(Clone)]
pub struct ComposedSet {
    pub(crate) operations: BTreeMap<String, Arc<ComposedOperation>>,
    pub(crate) identity: Arc<()>,
    pub(crate) gate: Arc<dyn Gate>,
    pub(crate) execution: ExecutionMode,
    revision: String,
    bounds: Bounds,
}

impl fmt::Debug for ComposedSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ComposedSet")
            .field("operations", &self.operations)
            .field("revision", &self.revision)
            .field("bounds", &self.bounds)
            .field("execution", &self.execution)
            .finish_non_exhaustive()
    }
}

impl ComposedSet {
    /// Descriptors in complete-name order, independent of contribution order.
    pub fn operations(&self) -> impl ExactSizeIterator<Item = &Operation> {
        self.operations
            .values()
            .map(|operation| operation.descriptor())
    }
    /// A compiled operation by its complete name.
    /// This accessor applies no selection or admission policy. Use [`Self::call`]
    /// for caller-facing dispatch; low-level `clapham.describe` invocation considers
    /// the full gate-filtered catalogue.
    pub fn get(&self, name: &str) -> Option<&ComposedOperation> {
        self.operations.get(name).map(Arc::as_ref)
    }
    /// A versioned SHA-256 digest of canonical descriptors in complete-name order.
    pub fn revision(&self) -> &str {
        &self.revision
    }
    /// The finite limits used to build this set.
    pub fn bounds(&self) -> &Bounds {
        &self.bounds
    }
}

/// Compose namespaces into an immutable set, compiling each schema once.
///
/// Every set includes `clapham.describe` and `clapham.version`; their namespace is
/// reserved for the built-in contribution. Names must belong to their
/// contribution's namespace, and complete names must be unique. Schema references
/// must resolve within their document; composition never fetches a file or URL.
pub fn compose(
    contributions: impl IntoIterator<Item = Contribution>,
    bounds: Bounds,
) -> Result<ComposedSet, CompositionError> {
    compose_with_options(contributions, bounds, ComposeOptions::default())
}

/// Compose with a host-supplied admission gate and execution mode.
/// These options are immutable and shared by clones of the resulting set.
pub fn compose_with_options(
    contributions: impl IntoIterator<Item = Contribution>,
    bounds: Bounds,
    options: ComposeOptions,
) -> Result<ComposedSet, CompositionError> {
    bounds.validate().map_err(|error| {
        CompositionError::new(CompositionCode::InvalidBounds, error.to_string())
    })?;
    let mut namespaces = BTreeSet::new();
    let mut operations = BTreeMap::new();
    let mut catalogue_remaining = bounds.catalogue_bytes.checked_sub(2).ok_or_else(|| {
        CompositionError::new(
            CompositionCode::CatalogueTooLarge,
            "descriptor catalogue exceeds bounds.catalogue_bytes",
        )
    })?;
    for contribution in contributions {
        let namespace = contribution.namespace;
        if namespace.is_empty()
            || namespace.len() > 128
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(CompositionError::new(
                CompositionCode::InvalidName,
                format!("invalid namespace {namespace:?}"),
            ));
        }
        if namespace == "clapham" || !namespaces.insert(namespace.clone()) {
            return Err(CompositionError::new(
                CompositionCode::NamespaceClaimed,
                format!("namespace {namespace:?} is already claimed"),
            ));
        }
        let binding_revision = contribution.binding_revision.map(Arc::<str>::from);
        for (descriptor, handler) in contribution.operations {
            insert_operation(
                &mut operations,
                &namespace,
                descriptor,
                Dispatch::Handler(handler),
                binding_revision.clone(),
                &bounds,
                &mut catalogue_remaining,
            )?;
        }
    }
    let describe = crate::builtins::Describe::new(options.gate.clone(), bounds.message_bytes);
    for (descriptor, handler) in crate::builtins::operations(describe.clone(), options.application)
    {
        insert_operation(
            &mut operations,
            "clapham",
            descriptor,
            handler,
            None,
            &bounds,
            &mut catalogue_remaining,
        )?;
    }
    describe.initialise(
        operations
            .values()
            .map(|operation| operation.descriptor.clone())
            .collect(),
    );
    let descriptors: Vec<_> = operations
        .values()
        .map(|operation| operation.descriptor())
        .collect();
    let revision = crate::revision::digest(b"clapham.composed-set.v1\0", descriptors);
    Ok(ComposedSet {
        operations,
        identity: Arc::new(()),
        gate: options.gate,
        execution: options.execution,
        revision,
        bounds,
    })
}

fn insert_operation(
    operations: &mut BTreeMap<String, Arc<ComposedOperation>>,
    namespace: &str,
    descriptor: Operation,
    handler: Dispatch,
    binding_revision: Option<Arc<str>>,
    bounds: &Bounds,
    catalogue_remaining: &mut usize,
) -> Result<(), CompositionError> {
    let name = descriptor.name.as_str();
    if descriptor.name.namespace() != Some(namespace) || descriptor.name.local_name().is_empty() {
        return Err(CompositionError::new(
            CompositionCode::InvalidName,
            format!("operation {name:?} must have a local name in namespace {namespace:?}"),
        ));
    }
    if descriptor.description.trim().is_empty() {
        return Err(CompositionError::new(
            CompositionCode::MissingDescription,
            format!("operation {name:?} has no description"),
        ));
    }
    if operations.contains_key(name) {
        return Err(CompositionError::new(
            CompositionCode::DuplicateOperation,
            format!("operation {name:?} is repeated"),
        ));
    }
    if operations.len() >= bounds.tools {
        return Err(CompositionError::new(
            CompositionCode::TooManyOperations,
            "operation count exceeds bounds.tools",
        ));
    }
    if !fits(&descriptor.input, bounds.schema_bytes)
        || descriptor
            .output
            .as_ref()
            .is_some_and(|schema| !fits(schema, bounds.schema_bytes))
    {
        return Err(CompositionError::new(
            CompositionCode::SchemaTooLarge,
            format!("operation {name:?} exceeds bounds.schema_bytes"),
        ));
    }
    if descriptor.input.get("type").and_then(Value::as_str) != Some("object") {
        return Err(CompositionError::new(
            CompositionCode::InvalidSchema,
            format!("operation {name:?} must declare type: object at its input root"),
        ));
    }
    let remaining = catalogue_remaining
        .checked_sub(usize::from(!operations.is_empty()))
        .and_then(|remaining| size_within(&descriptor, remaining).map(|size| remaining - size))
        .ok_or_else(|| {
            CompositionError::new(
                CompositionCode::CatalogueTooLarge,
                "descriptor catalogue exceeds bounds.catalogue_bytes",
            )
        })?;
    // Charge metadata before retaining any more compiled validators.
    *catalogue_remaining = remaining;
    let input = compile(&Value::Object(descriptor.input.clone()), name)?;
    let output = descriptor
        .output
        .as_ref()
        .map(|schema| compile(schema.as_value(), name))
        .transpose()?;
    operations.insert(
        name.to_owned(),
        Arc::new(ComposedOperation {
            descriptor: Arc::new(descriptor),
            input,
            output,
            handler,
            binding_revision,
        }),
    );
    Ok(())
}

fn compile(schema: &Value, name: &str) -> Result<jsonschema::Validator, CompositionError> {
    jsonschema::options()
        .offline()
        .build(schema)
        .map_err(|error| {
            CompositionError::new(
                CompositionCode::InvalidSchema,
                format!("operation {name:?}: {error}"),
            )
        })
}

fn fits(value: &impl Serialize, limit: usize) -> bool {
    size_within(value, limit).is_some()
}

pub(crate) fn size_within(value: &impl Serialize, limit: usize) -> Option<usize> {
    struct Counter {
        remaining: usize,
    }
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("byte bound exceeded"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { remaining: limit };
    serde_json::to_writer(&mut counter, value).ok()?;
    Some(limit - counter.remaining)
}
