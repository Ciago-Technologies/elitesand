//! Bounded actor-native execution components. Owner-local state; no server or durable-ACK claim.
#![deny(unsafe_op_in_unsafe_fn, unsafe_code)]
// flatc 25 emits pre-2024 unsafe function bodies; only verified roots are exposed.
#[allow(
    unused_imports,
    dead_code,
    clippy::all,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    mismatched_lifetime_syntaxes
)]
mod actor_generated;
pub mod image;
pub mod index;
pub mod join;
pub mod links;
pub mod memory;
mod model;
mod runtime;
pub mod schema;
pub use model::*;
pub use runtime::*;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Budget,
    InvalidValue,
    InvalidImage,
    Type,
    MissingActor,
    StaleReference,
    ForeignActor,
    Permission,
    Unsupported,
    Overflow,
    Restricted,
    Duplicate,
    MissingSchema,
    Journal,
    Parse,
    InvalidQuery,
    AlreadyExists,
    IdempotencyConflict,
    RetryTooOld,
    SequenceGap,
    DedupCapacity,
    ConditionFailed,
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(unix)]
pub mod persistence;

#[cfg(unix)]
pub mod host;

#[cfg(unix)]
pub mod retry;

#[cfg(unix)]
pub mod wire;

#[cfg(target_os = "linux")]
pub mod network;

#[cfg(target_os = "linux")]
pub mod client;
