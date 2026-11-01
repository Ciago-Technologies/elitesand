//! WAL envelopes and per-core group-commit logging.
pub use ciagomem_core::envelope;
pub mod gclog;

#[cfg(unix)]
pub mod driver;
