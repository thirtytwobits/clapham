use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::{
    Admission, Call, Cancellation, Category, ComposedOperation, ComposedSet, Evidence,
    ExecutionMode, Locality, OAuthContext, Operation, Outcome, RecordSink, Refusal,
};

/// The outcome or refusal of a call through a composed set.
pub type CallResult = Result<Outcome, Refusal>;

/// An immutable subset of one composed set, with its binding-aware revision.
#[derive(Debug, Clone)]
pub struct Selection {
    operations: BTreeMap<String, Arc<ComposedOperation>>,
    origin: Arc<()>,
    revision: String,
}

impl Selection {
    fn new(operations: BTreeMap<String, Arc<ComposedOperation>>, origin: Arc<()>) -> Self {
        let descriptors: Vec<_> = operations
            .values()
            .map(|operation| operation.descriptor())
            .collect();
        let bindings: BTreeMap<_, _> = operations
            .values()
            .filter_map(|operation| {
                operation.binding_revision.as_deref().map(|revision| {
                    (
                        operation
                            .descriptor()
                            .name
                            .namespace()
                            .expect("composed names have a namespace"),
                        revision,
                    )
                })
            })
            .collect();
        let revision = if bindings.is_empty() {
            crate::revision::digest(b"clapham.composed-set.v1\0", &descriptors)
        } else {
            crate::revision::digest(
                b"clapham.selection.v1\0",
                json!({ "operations": descriptors, "bindings": bindings }),
            )
        };
        Self {
            operations,
            origin,
            revision,
        }
    }

    /// Selected descriptors in complete-name order.
    pub fn operations(&self) -> impl ExactSizeIterator<Item = &Operation> {
        self.operations
            .values()
            .map(|operation| operation.descriptor())
    }

    /// Whether the complete operation name is selected.
    pub fn contains(&self, name: &str) -> bool {
        self.operations.contains_key(name)
    }

    /// The selected descriptors and relevant remote binding revisions, in stable order.
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

impl ComposedSet {
    /// Select complete operation names; unknown names refuse as `unknown_operation`.
    /// Repeated names are deduplicated. Selections remain valid for clones of this set.
    pub fn select<I, S>(&self, names: I) -> Result<Selection, Refusal>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut selected = BTreeMap::new();
        for name in names {
            let name = name.as_ref();
            let operation = self.operations.get(name).ok_or_else(unknown_operation)?;
            selected.insert(name.to_owned(), operation.clone());
        }
        Ok(Selection::new(selected, self.identity.clone()))
    }

    /// Select every operation in this set.
    pub fn all(&self) -> Selection {
        Selection::new(self.operations.clone(), self.identity.clone())
    }

    /// Validate, admit and run a selected operation under verified OAuth authority.
    ///
    /// Already-cancelled calls return before any validation or admission. Otherwise,
    /// the order is selection, resource binding, scopes, input conversion, gate,
    /// preview support, locality, resource policies, dispatch and output validation,
    /// with cancellation checked again before dispatch.
    /// Every pre-dispatch refusal carries not-dispatched evidence. A selection from
    /// another independently composed set cannot authorise a call here, even if the
    /// descriptors and revision happen to match.
    #[allow(clippy::too_many_arguments)]
    pub async fn call(
        &self,
        selection: &Selection,
        auth: OAuthContext,
        name: &str,
        input: Value,
        sink: RecordSink,
        cancel: Cancellation,
        preview: bool,
    ) -> CallResult {
        if cancel.is_cancelled() {
            return Err(crate::composition::cancelled(Evidence::NotDispatched));
        }
        if !Arc::ptr_eq(&self.identity, &selection.origin) || !selection.contains(name) {
            return Err(unknown_operation());
        }
        let operation = self.get(name).ok_or_else(unknown_operation)?;
        let descriptor = operation.descriptor();
        if self
            .oauth
            .as_ref()
            .is_some_and(|resource| !auth.is_for(resource))
        {
            return Err(Refusal::not_dispatched(
                "not_authenticated",
                Category::Refused,
                "the verified grant is not bound to this resource server",
            ));
        }
        if !descriptor.scopes.iter().all(|scope| auth.has_scope(scope)) {
            let mut refusal = Refusal::not_dispatched(
                "insufficient_scope",
                Category::Refused,
                "the grant lacks required OAuth scopes",
            );
            refusal.details = Some(json!({"required_scopes": descriptor.scopes}));
            return Err(refusal);
        }
        operation.validate_input(&input)?;
        match self.gate.admit(&auth, descriptor) {
            Admission::Allow => {}
            Admission::Deny => {
                return Err(Refusal::not_dispatched(
                    "not_admitted",
                    Category::Refused,
                    "the gate denied this call",
                ));
            }
            Admission::Pending => {
                let mut refusal = Refusal::not_dispatched(
                    "admission_pending",
                    Category::Refused,
                    "approval is pending",
                );
                refusal.retryable = true;
                return Err(refusal);
            }
        }
        if preview && !descriptor.preview {
            return Err(Refusal::not_dispatched(
                "preview_unsupported",
                Category::Usage,
                "the operation does not support a dry run",
            ));
        }
        if self.execution == ExecutionMode::InProcess && descriptor.locality == Locality::Daemon {
            return Err(Refusal::not_dispatched(
                "no_daemon",
                Category::Unavailable,
                "the operation needs the running daemon",
            ));
        }
        for policy in &operation.policies {
            use futures_util::future::{Either, select};
            match select(
                Box::pin(cancel.cancelled()),
                policy.check(auth.clone(), input.clone()),
            )
            .await
            {
                Either::Left(_) => {
                    return Err(crate::composition::cancelled(Evidence::NotDispatched));
                }
                Either::Right((result, _)) => {
                    if let Err(refusal) = result.map_err(Refusal::from)? {
                        return Err(declared_refusal(descriptor, refusal));
                    }
                }
            }
        }
        // The host's sink refusals are not the operation's to declare. Remember the last
        // so a handler that returns it passes it to the caller unchanged.
        let refused = Arc::new(Mutex::new(None));
        let records = {
            let refused = refused.clone();
            RecordSink::new(move |record| {
                let (sink, refused) = (sink.clone(), refused.clone());
                async move {
                    let delivered = sink.send(record).await;
                    if let Err(refusal) = &delivered {
                        *refused.lock().expect("sink refusal lock") = Some(refusal.clone());
                    }
                    delivered
                }
            })
        };
        let call = Call {
            name: descriptor.name.clone(),
            auth,
            input,
            records,
            cancel,
            preview,
        };
        // The outer error is the dispatcher's cancellation. The inner refusal
        // belongs to the handler, even if it uses a dispatcher-like code, unless
        // it is the refusal the host's sink returned.
        let outcome = match operation.dispatch_validated(call, Some(selection)).await? {
            Ok(outcome) => outcome,
            Err(refusal)
                if refused.lock().expect("sink refusal lock").as_ref() == Some(&refusal) =>
            {
                return Err(refusal);
            }
            Err(refusal) => return Err(declared_refusal(descriptor, refusal)),
        };
        operation.validate_output(&outcome)?;
        Ok(outcome)
    }
}

fn declared_refusal(descriptor: &crate::Operation, mut refusal: Refusal) -> Refusal {
    if let Some(category) = descriptor.refusals.get(&refusal.code) {
        refusal.category = *category;
        return refusal;
    }
    let mut undeclared = Refusal::new(
        "undeclared_refusal",
        Category::Internal,
        "the handler or resource policy returned a refusal code its descriptor does not declare",
    )
    .with_evidence(refusal.evidence);
    undeclared.details =
        Some(serde_json::to_value(refusal).expect("refusals contain only JSON values"));
    undeclared
}

fn unknown_operation() -> Refusal {
    Refusal::not_dispatched(
        "unknown_operation",
        Category::NotFound,
        "the operation is not in this set's selection",
    )
}
