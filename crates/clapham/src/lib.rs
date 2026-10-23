#![doc = include_str!("../README.md")]

mod bounds;
mod builtins;
mod call;
mod composition;
mod gate;
mod name;
mod operation;
mod principal;
mod refusal;
mod revision;
pub mod schema;
mod selection;

pub use bounds::{Bounds, InvalidBounds};
pub use builtins::{ApplicationInfo, Catalogue, VersionInfo};
pub use call::{
    Call, Cancellation, ContentBlock, Evidence, Handler, HandlerFuture, Outcome, RecordSink,
    ResourceContents,
};
pub use composition::{
    ComposedOperation, ComposedSet, CompositionCode, CompositionError, Contribution, compose,
    compose_with_options,
};
pub use gate::{Admission, ComposeOptions, DefaultGate, ExecutionMode, Gate};
pub use name::{InvalidOperationName, OperationName};
pub use operation::{Annotations, EFFECT_META_KEY, Effect, Locality, Operation};
pub use principal::{Principal, PrincipalKind};
pub use refusal::{Category, Refusal};
pub use selection::{CallResult, Selection};
