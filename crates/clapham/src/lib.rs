#![doc = include_str!("../README.md")]

mod bounds;
mod call;
mod composition;
mod name;
mod operation;
mod principal;
mod refusal;
pub mod schema;

pub use bounds::{Bounds, InvalidBounds};
pub use call::{
    Call, Cancellation, ContentBlock, Evidence, Handler, HandlerFuture, Outcome, RecordSink,
    ResourceContents,
};
pub use composition::{
    ComposedOperation, ComposedSet, CompositionCode, CompositionError, Contribution, compose,
};
pub use name::{InvalidOperationName, OperationName};
pub use operation::{Annotations, EFFECT_META_KEY, Effect, Locality, Operation};
pub use principal::{Principal, PrincipalKind};
pub use refusal::{Category, Refusal};
