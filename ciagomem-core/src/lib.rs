//! Ciagomem component kernel. Canonical storage types are the persistence-wired P0 family.
#![allow(clippy::too_many_arguments)]
pub use calvin::{Participants, Vote, VoteTable};
pub use seqlock::{
    READ_RETRY_LIMIT, ReadError, SeqWord, SeqWriteGuard, WordView, WordsMut, WriterBusy,
    read_deadline, read_with,
};
pub use slot::{
    CoreControl, ExtentLoc, SlotArena, SlotHandle, SlotHeader, SlotReadError, Snapshot, TenantWord,
    Visibility,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineReadError {
    DeadlineExhausted { retries: u32 },
    WriterStalled { odd_seq: u64 },
    Corrupt,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    WriterBusy,
    Reassigned,
    LsnRegression { current: u64, attempted: u64 },
    DuplicateLsn,
}
pub trait Clock {
    fn now_ns(&self) -> u64;
}
pub struct MonotonicClock;
impl Clock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos()
            .min(u64::MAX as u128) as u64
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    None,
    FreezeCheckpoint { tenant: u32 },
    Relocate { tenant: u32 },
}
pub mod dirty;
pub use dirty::TruncationPins;
pub mod flow;
pub use flow::*;
pub mod security;
pub use ciagomem_crypto::*;
pub use security::*;
pub mod policy;
pub use ciagomem_ql::{ParseError, Query as CiaQuery, Source, Stage, parse as parse_ciaql};
pub use policy::*;
pub mod calvin;
#[path = "../../ciagomem-wal/src/envelope.rs"]
pub mod envelope;
#[cfg(target_os = "linux")]
pub mod export_window;
pub mod io;
#[cfg(feature = "legacy-audit")]
pub mod legacy;
pub mod lifecycle;
pub mod mesh;
pub mod seqlock;
pub mod slot;
#[cfg(target_os = "linux")]
pub mod uring;
#[derive(Debug, Default)]
#[repr(C, align(128))]
pub struct CachePadded<T>(pub T);
impl<T> core::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(target_os = "linux")]
pub mod destination;
