#![doc = include_str!("../README.md")]

mod auth;
mod bounds;
mod builtins;
mod call;
mod composition;
mod configuration;
mod gate;
mod name;
mod operation;
mod policy;
mod refusal;
mod revision;
pub mod schema;
mod selection;
mod typed;

pub use auth::{OAuthContext, OAuthResource};
pub use bounds::{Bounds, InvalidBounds};
pub use builtins::{ApplicationInfo, Catalogue, VersionInfo};
pub use call::{
    Call, Cancellation, CheckedHandlerFuture, ConversionFailure, Evidence, Handler, HandlerFuture,
    Outcome, RecordSink,
};
pub use composition::{
    ComposedOperation, ComposedSet, CompositionCode, CompositionError, Contribution, compose,
    compose_with_options,
};
pub use configuration::{CliSettings, Configuration, Exposure, Requirements, Source};
pub use gate::{Admission, ComposeOptions, ExecutionMode, Gate, ScopeGate};
pub use name::{InvalidOperationName, OperationName};
pub use operation::{Effect, Locality, Operation};
pub use policy::ResourcePolicy;
pub use refusal::{Category, Refusal};
pub use selection::{CallResult, Selection};
pub use typed::{TypedOperation, TypedOutcome};
