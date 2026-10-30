//! CiagoMem - SLAE ("The Store") & CiaQL — v3.0 Final Audit-Hardened Reference Kernel.
//! Reviewed component kernel; production integration and qualification remain open.

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{
    AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
    fence,
};
use std::time::Duration;

// =============================================================================
// 1. PROT_READ-SAFE BOEHM SEQLOCK + DEADLINE + POISON DETECTION + READ COMBINER
// =============================================================================

pub const READ_RETRY_LIMIT: u32 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    WriterActive,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineReadError {
    DeadlineExhausted { retries: u32 },
    WriterStalled { odd_seq: u64 },
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterBusy;

pub trait Clock {
    fn now_ns(&self) -> u64;
}

/// Portable monotonic clock; no calibrated CPU-counter latency claim.
pub struct MonotonicClock;
impl Clock for MonotonicClock {
    #[inline(always)]
    fn now_ns(&self) -> u64 {
        // Monotonic nanosecond clock portable across x86_64, aarch64, and Miri.
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos() as u64
    }
}

#[derive(Debug, Default)]
#[repr(transparent)]
pub struct SeqWord(AtomicU64);

impl SeqWord {
    pub const fn new() -> Self {
        SeqWord(AtomicU64::new(0))
    }

    #[inline(always)]
    pub fn read_begin(&self) -> Option<u64> {
        let s = self.0.load(Relaxed);
        fence(Acquire); // F1: orders subsequent relaxed data word loads
        if s & 1 == 0 { Some(s) } else { None }
    }

    #[inline(always)]
    pub fn read_retry(&self, s1: u64) -> bool {
        fence(Acquire); // F2: prevents data word loads from sinking past s2
        self.0.load(Relaxed) != s1
    }

    #[inline(always)]
    pub fn write_begin(&self) -> Result<SeqWriteGuard<'_>, WriterBusy> {
        let s = self.0.load(Relaxed);
        if s & 1 != 0 {
            return Err(WriterBusy);
        }
        if self.0.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
            return Err(WriterBusy);
        }
        fence(Release); // W1: orders odd sequence store before data stores
        Ok(SeqWriteGuard {
            seq: self,
            odd: s + 1,
            committed: false,
        })
    }

    #[inline(always)]
    pub fn raw(&self) -> u64 {
        self.0.load(Relaxed)
    }
}

#[must_use = "dropping SeqWriteGuard without commit() leaves an odd sequence; recovery requires owner intervention"]
pub struct SeqWriteGuard<'a> {
    seq: &'a SeqWord,
    odd: u64,
    committed: bool,
}

impl SeqWriteGuard<'_> {
    #[inline(always)]
    pub fn commit(mut self) {
        self.committed = true;
        self.seq.0.store(self.odd + 1, Release); // W2: publishes section
    }
}

impl Drop for SeqWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            debug_assert!(self.seq.raw() & 1 == 1);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WordView<'a> {
    words: &'a [AtomicU64],
}

impl<'a> WordView<'a> {
    #[inline(always)]
    pub fn new(words: &'a [AtomicU64]) -> Self {
        WordView { words }
    }

    #[inline(always)]
    pub fn word(&self, i: usize) -> Option<u64> {
        self.words.get(i).map(|w| w.load(Relaxed))
    }

    #[inline]
    pub fn copy_bytes(&self, byte_off: usize, dst: &mut [u8]) -> Option<()> {
        let end = byte_off.checked_add(dst.len())?;
        if end > self.words.len() * 8 {
            return None;
        }
        let (mut out, mut off) = (0, byte_off);
        while out < dst.len() {
            let w = self.words[off / 8].load(Relaxed).to_le_bytes();
            let in_word = off % 8;
            let n = (8 - in_word).min(dst.len() - out);
            dst[out..out + n].copy_from_slice(&w[in_word..in_word + n]);
            out += n;
            off += n;
        }
        Some(())
    }

    #[inline]
    pub fn u32_at(&self, byte_off: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        self.copy_bytes(byte_off, &mut b)?;
        Some(u32::from_le_bytes(b))
    }
}

pub struct WordsMut<'a> {
    words: &'a [AtomicU64],
}

impl<'a> WordsMut<'a> {
    #[inline(always)]
    pub fn set(&mut self, i: usize, val: u64) {
        self.words[i].store(val, Relaxed);
    }

    #[inline]
    pub fn write_bytes(&mut self, byte_off: usize, src: &[u8]) {
        let (mut inp, mut off) = (0, byte_off);
        while inp < src.len() {
            let idx = off / 8;
            let in_word = off % 8;
            let n = (8 - in_word).min(src.len() - inp);
            let mut w = self.words[idx].load(Relaxed).to_le_bytes();
            w[in_word..in_word + n].copy_from_slice(&src[inp..inp + n]);
            self.words[idx].store(u64::from_le_bytes(w), Relaxed);
            inp += n;
            off += n;
        }
    }
}

#[inline]
pub fn read_with<R>(
    seq: &SeqWord,
    words: &[AtomicU64],
    budget: u32,
    mut f: impl FnMut(WordView<'_>) -> Option<R>,
) -> Result<R, ReadError> {
    let view = WordView::new(words);
    let mut backoff = 1u32;
    for _ in 0..budget {
        if let Some(s1) = seq.read_begin() {
            let r = f(view);
            if !seq.read_retry(s1) {
                return r.ok_or(ReadError::Corrupt);
            }
        }
        for _ in 0..backoff {
            core::hint::spin_loop();
        }
        backoff = (backoff << 1).min(64);
    }
    Err(ReadError::WriterActive)
}

/// Time-budgeted seqlock reader with poison detection (Fixes H2).
pub fn read_deadline<R>(
    seq: &SeqWord,
    words: &[AtomicU64],
    clock: &impl Clock,
    budget: Duration,
    stall_after: Duration,
    mut f: impl FnMut(WordView<'_>) -> Option<R>,
) -> Result<R, DeadlineReadError> {
    let start = clock.now_ns();
    let deadline = start.saturating_add(budget.as_nanos().min(u64::MAX as u128) as u64);
    let stall_ns = stall_after.as_nanos().min(u64::MAX as u128) as u64;
    let view = WordView::new(words);

    let mut odd_seen: Option<(u64, u64)> = None;
    let mut retries = 0u32;
    let mut spins = 1u32;

    loop {
        if let Some(s1) = seq.read_begin() {
            odd_seen = None;
            let r = f(view);
            if !seq.read_retry(s1) {
                return r.ok_or(DeadlineReadError::Corrupt);
            }
        } else {
            let raw = seq.raw();
            let now = clock.now_ns();
            match odd_seen {
                Some((prev, since)) if prev == raw => {
                    if now.saturating_sub(since) >= stall_ns {
                        return Err(DeadlineReadError::WriterStalled { odd_seq: raw });
                    }
                }
                _ => odd_seen = Some((raw, now)),
            }
        }
        retries += 1;
        if clock.now_ns() >= deadline {
            return Err(DeadlineReadError::DeadlineExhausted { retries });
        }
        for _ in 0..spins {
            core::hint::spin_loop();
        }
        spins = (spins << 1).min(32);
    }
}

/// Owner-side read combiner: collapses N fallback reads on one slot into 1 snapshot (Fixes H2).
#[derive(Default)]
pub struct ReadCombiner {
    waiters: HashMap<u32, Vec<u64>>,
    pub snapshots_taken: u64,
    pub requests_served: u64,
}

impl ReadCombiner {
    pub fn enqueue(&mut self, slot: u32, req_id: u64) {
        self.waiters.entry(slot).or_default().push(req_id);
    }

    pub fn flush<R: Clone>(
        &mut self,
        mut snap: impl FnMut(u32) -> R,
        mut reply: impl FnMut(u64, R),
    ) {
        for (slot, reqs) in self.waiters.drain() {
            if reqs.is_empty() {
                continue;
            }
            let v = snap(slot);
            self.snapshots_taken += 1;
            for id in reqs {
                self.requests_served += 1;
                reply(id, v.clone());
            }
        }
    }
}

// =============================================================================
// 2. NON-BLOCKING CREDIT-CONTROLLED SPSC MESH (Fixes P0-1b & H1)
// =============================================================================

pub trait Lane<T> {
    fn try_push(&mut self, v: T) -> Result<(), T>;
}

impl<T> Lane<T> for (VecDeque<T>, usize) {
    fn try_push(&mut self, v: T) -> Result<(), T> {
        if self.0.len() >= self.1 {
            Err(v)
        } else {
            self.0.push_back(v);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    Deferred,
    ShedAdmission,
}

pub struct OutboundFlow<T> {
    credits: usize,
    overflow: VecDeque<T>,
    cap: usize,
    pub overflow_hwm: usize,
    lane_capacity: usize,
    pub excess_credits: usize,
}

impl<T> OutboundFlow<T> {
    pub fn new(lane_capacity: usize, overflow_cap: usize) -> Self {
        OutboundFlow {
            credits: lane_capacity,
            overflow: VecDeque::with_capacity(overflow_cap),
            cap: overflow_cap,
            overflow_hwm: 0,
            lane_capacity,
            excess_credits: 0,
        }
    }

    pub fn send(&mut self, lane: &mut impl Lane<T>, v: T) -> Result<SendOutcome, T> {
        if self.overflow.len() >= self.cap && !(self.overflow.is_empty() && self.credits > 0) {
            return Err(v);
        }
        if self.overflow.is_empty() && self.credits > 0 {
            match lane.try_push(v) {
                Ok(()) => {
                    self.credits -= 1;
                    return Ok(SendOutcome::Sent);
                }
                Err(v) => {
                    if self.overflow.len() >= self.cap {
                        return Err(v);
                    }
                    self.overflow.push_back(v);
                }
            }
        } else {
            self.overflow.push_back(v);
        }
        self.overflow_hwm = self.overflow_hwm.max(self.overflow.len());
        if self.overflow.len() >= self.cap {
            Ok(SendOutcome::ShedAdmission)
        } else {
            Ok(SendOutcome::Deferred)
        }
    }

    pub fn on_credits(&mut self, c: usize) {
        let available = self.lane_capacity - self.credits;
        self.excess_credits = self
            .excess_credits
            .saturating_add(c.saturating_sub(available));
        self.credits += c.min(available);
    }

    pub fn pump(&mut self, lane: &mut impl Lane<T>) -> usize {
        let mut sent = 0;
        while self.credits > 0 {
            let Some(v) = self.overflow.pop_front() else {
                break;
            };
            match lane.try_push(v) {
                Ok(()) => {
                    self.credits -= 1;
                    sent += 1;
                }
                Err(v) => {
                    self.overflow.push_front(v);
                    break;
                }
            }
        }
        sent
    }

    pub fn backlog(&self) -> usize {
        self.overflow.len()
    }
}

// =============================================================================
// 3. SPECTRE-SAFE ROUTE TABLE, SLOT HANDLES & CHECKED EXTENT LOCATORS
// =============================================================================

pub const IDX_BITS: u32 = 20;
pub const CLASS_BITS: u32 = 4;
pub const GEN_BITS: u32 = 64 - IDX_BITS - CLASS_BITS; // 40 bits
pub const MAX_INDEX: u32 = (1 << IDX_BITS) - 1;
pub const MAX_CLASS: u8 = (1 << CLASS_BITS) - 1;
pub const MAX_GENERATION: u64 = (1 << GEN_BITS) - 1;
const IDX_MASK: u64 = (1 << IDX_BITS) - 1;
const CLASS_SHIFT: u32 = IDX_BITS;
const CLASS_MASK: u64 = ((1 << CLASS_BITS) - 1) << CLASS_SHIFT;
const GEN_SHIFT: u32 = IDX_BITS + CLASS_BITS;

/// Architectural bounds check. This Rust expression is not a proven speculation barrier.
#[inline(always)]
pub const fn nospec_index(idx: usize, len: usize) -> Option<usize> {
    if idx < len {
        let mask = ((idx < len) as usize).wrapping_neg();
        Some(idx & mask)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct SlotHandle(u64);

impl SlotHandle {
    pub const NULL: SlotHandle = SlotHandle(0);

    #[inline(always)]
    pub const fn pack(index: u32, class: u8, generation: u64) -> Option<SlotHandle> {
        if index > MAX_INDEX || class > MAX_CLASS || generation == 0 || generation > MAX_GENERATION
        {
            return None;
        }
        Some(SlotHandle(
            (generation << GEN_SHIFT) | ((class as u64) << CLASS_SHIFT) | (index as u64),
        ))
    }

    #[inline(always)]
    pub const fn index(self) -> u32 {
        (self.0 & IDX_MASK) as u32
    }

    #[inline(always)]
    pub const fn class(self) -> u8 {
        ((self.0 & CLASS_MASK) >> CLASS_SHIFT) as u8
    }

    #[inline(always)]
    pub const fn generation(self) -> u64 {
        self.0 >> GEN_SHIFT
    }

    #[inline(always)]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantWord {
    pub tenant: u32,
    pub core: u8,
    pub layout_version: u8,
    pub flags: u16,
}

impl TenantWord {
    #[inline(always)]
    pub const fn pack(self) -> u64 {
        (self.tenant as u64)
            | ((self.core as u64) << 32)
            | ((self.layout_version as u64) << 40)
            | ((self.flags as u64) << 48)
    }

    #[inline(always)]
    pub const fn unpack(w: u64) -> Self {
        TenantWord {
            tenant: w as u32,
            core: (w >> 32) as u8,
            layout_version: (w >> 40) as u8,
            flags: (w >> 48) as u16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct ExtentLoc(u64);

impl ExtentLoc {
    pub const MAX_CONTAINER: u16 = (1 << 12) - 1;
    pub const MAX_BLOCK_4K: u64 = (1u64 << 48) - 1;

    /// Checked constructor: rejects out-of-range container or block_4k (Fixes M1).
    #[inline(always)]
    pub const fn new_checked(container: u16, copy_b: bool, block_4k: u64) -> Option<Self> {
        if container > Self::MAX_CONTAINER || block_4k > Self::MAX_BLOCK_4K {
            return None;
        }
        Some(ExtentLoc(
            (block_4k & Self::MAX_BLOCK_4K) | ((copy_b as u64) << 51) | ((container as u64) << 52),
        ))
    }

    #[inline(always)]
    pub const fn flipped(self) -> Self {
        ExtentLoc(self.0 ^ (1u64 << 51))
    }

    #[inline(always)]
    pub const fn is_copy_b(self) -> bool {
        ((self.0 >> 51) & 1) != 0
    }

    #[inline(always)]
    pub const fn container(self) -> u16 {
        (self.0 >> 52) as u16
    }

    #[inline(always)]
    pub const fn block_4k(self) -> u64 {
        self.0 & Self::MAX_BLOCK_4K
    }

    #[inline(always)]
    pub const fn current_offset(self, extent_bytes: u64) -> u64 {
        let base = self.block_4k() * 4096;
        if self.is_copy_b() {
            base + extent_bytes
        } else {
            base
        }
    }

    #[inline(always)]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[repr(C, align(64))]
pub struct SlotHeader {
    pub seq: SeqWord,
    pub owner: AtomicU64,
    pub tenant: AtomicU64,
    pub applied_lsn: AtomicU64,
    pub checkpoint_lsn: AtomicU64,
    pub payload_len: AtomicU64,
    pub last_generation: AtomicU64,
    pub incarnation_lo: AtomicU64,
    pub incarnation_hi: AtomicU64,
}
const _: () = assert!(core::mem::size_of::<SlotHeader>() == 128);

#[repr(C, align(64))]
pub struct CoreControl {
    durable_lsn: AtomicU64,
    _pad: [u64; 7],
}

impl Default for CoreControl {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreControl {
    pub const fn new() -> Self {
        CoreControl {
            durable_lsn: AtomicU64::new(0),
            _pad: [0; 7],
        }
    }

    #[inline(always)]
    pub fn durable(&self) -> u64 {
        let d = self.durable_lsn.load(Relaxed);
        fence(Acquire);
        d
    }

    /// Monotonic publication using `fetch_max` so durable_lsn can never regress (Fixes M3).
    #[inline(always)]
    pub fn publish_durable(&self, lsn: u64) -> u64 {
        self.durable_lsn.fetch_max(lsn, Release).max(lsn)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Optimistic,
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotReadError {
    InvalidHandle,
    Reassigned,
    NotYetDurable { applied: u64, durable: u64 },
    WriterStalled,
    DeadlineExhausted,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    WriterBusy,
    Reassigned,
    LsnRegression { current: u64, attempted: u64 },
}

pub struct Snapshot<T> {
    pub value: T,
    pub applied_lsn: u64,
}

pub struct SlotArena<const W: usize> {
    class: u8,
    headers: Box<[SlotHeader]>,
    payloads: Box<[[AtomicU64; W]]>,
    pub cores: Box<[CoreControl]>,
}

impl<const W: usize> SlotArena<W> {
    pub const PAYLOAD_BYTES: usize = W * 8;

    pub fn new(class: u8, slots: usize, num_cores: usize) -> Self {
        let headers = (0..slots)
            .map(|_| SlotHeader {
                seq: SeqWord::new(),
                owner: AtomicU64::new(0),
                tenant: AtomicU64::new(0),
                applied_lsn: AtomicU64::new(0),
                checkpoint_lsn: AtomicU64::new(0),
                payload_len: AtomicU64::new(0),
                last_generation: AtomicU64::new(0),
                incarnation_lo: AtomicU64::new(0),
                incarnation_hi: AtomicU64::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let payloads = (0..slots)
            .map(|_| core::array::from_fn(|_| AtomicU64::new(0)))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let cores = (0..num_cores)
            .map(|_| CoreControl::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        SlotArena {
            class,
            headers,
            payloads,
            cores,
        }
    }

    #[inline(always)]
    pub fn resolve(&self, h: SlotHandle) -> Option<u32> {
        if h.raw() == 0 || h.class() != self.class {
            return None;
        }
        nospec_index(h.index() as usize, self.headers.len()).map(|i| i as u32)
    }

    #[inline(always)]
    pub fn header(&self, idx: u32) -> &SlotHeader {
        let i = nospec_index(idx as usize, self.headers.len()).expect("slot index out of bounds");
        &self.headers[i]
    }

    #[inline(always)]
    pub fn payload(&self, idx: u32) -> &[AtomicU64; W] {
        let i = nospec_index(idx as usize, self.payloads.len()).expect("slot index out of bounds");
        &self.payloads[i]
    }

    pub fn install(
        &self,
        idx: u32,
        tw: TenantWord,
        incarnation: u128,
        applied_lsn: u64,
        checkpoint_lsn: u64,
    ) -> Result<Option<SlotHandle>, WriterBusy> {
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        if hdr.owner.load(Relaxed) != 0 {
            g.commit();
            return Err(WriterBusy);
        }
        let last = hdr.last_generation.load(Relaxed);
        let Some(h) = SlotHandle::pack(idx, self.class, last + 1) else {
            g.commit();
            return Ok(None);
        };
        hdr.last_generation.store(h.generation(), Relaxed);
        hdr.tenant.store(tw.pack(), Relaxed);
        hdr.applied_lsn.store(applied_lsn, Relaxed);
        hdr.checkpoint_lsn.store(checkpoint_lsn, Relaxed);
        hdr.payload_len.store(Self::PAYLOAD_BYTES as u64, Relaxed);
        hdr.incarnation_lo.store(incarnation as u64, Relaxed);
        hdr.incarnation_hi
            .store((incarnation >> 64) as u64, Relaxed);
        hdr.owner.store(h.raw(), Relaxed);
        g.commit();
        Ok(Some(h))
    }

    pub fn revoke(&self, h: SlotHandle) -> Result<(), WriterBusy> {
        let idx = self.resolve(h).expect("invalid handle on owner core");
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        hdr.owner.store(0, Relaxed);
        hdr.tenant.store(0, Relaxed);
        hdr.incarnation_lo.store(0, Relaxed);
        hdr.incarnation_hi.store(0, Relaxed);
        g.commit();
        Ok(())
    }

    /// Validated write path: rejects LSN regression with WriteError::LsnRegression (Fixes M2).
    pub fn write(
        &self,
        h: SlotHandle,
        lsn: u64,
        f: impl FnOnce(&mut WordsMut<'_>),
    ) -> Result<(), WriteError> {
        let idx = self.resolve(h).ok_or(WriteError::Reassigned)?;
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin().map_err(|_| WriteError::WriterBusy)?;
        if hdr.owner.load(Relaxed) != h.raw() {
            g.commit();
            return Err(WriteError::Reassigned);
        }
        let current = hdr.applied_lsn.load(Relaxed);
        if lsn < current {
            g.commit();
            return Err(WriteError::LsnRegression {
                current,
                attempted: lsn,
            });
        }
        let mut wm = WordsMut {
            words: self.payload(idx),
        };
        f(&mut wm);
        hdr.applied_lsn.store(lsn, Relaxed);
        g.commit();
        Ok(())
    }

    pub fn read<R>(
        &self,
        h: SlotHandle,
        tenant: u32,
        vis: Visibility,
        mut f: impl FnMut(WordView<'_>) -> Option<R>,
    ) -> Result<Snapshot<R>, SlotReadError> {
        let idx = self.resolve(h).ok_or(SlotReadError::InvalidHandle)?;
        let hdr = self.header(idx);
        let words = self.payload(idx);
        let r = read_deadline(
            &hdr.seq,
            words,
            &MonotonicClock,
            Duration::from_micros(2),
            Duration::from_micros(1),
            |view| {
                let owner = hdr.owner.load(Relaxed);
                let tw = TenantWord::unpack(hdr.tenant.load(Relaxed));
                if owner != h.raw() || tw.tenant != tenant {
                    return Some(Err(()));
                }
                let applied = hdr.applied_lsn.load(Relaxed);
                Some(Ok((f(view)?, applied, tw.core)))
            },
        )
        .map_err(|e| match e {
            DeadlineReadError::WriterStalled { .. } => SlotReadError::WriterStalled,
            DeadlineReadError::DeadlineExhausted { .. } => SlotReadError::DeadlineExhausted,
            DeadlineReadError::Corrupt => SlotReadError::Corrupt,
        })?;
        let (value, applied, core) = r.map_err(|()| SlotReadError::Reassigned)?;
        if vis == Visibility::Durable {
            let durable = self
                .cores
                .get(core as usize)
                .ok_or(SlotReadError::Corrupt)?
                .durable();
            if applied > durable {
                return Err(SlotReadError::NotYetDurable { applied, durable });
            }
        }
        Ok(Snapshot {
            value,
            applied_lsn: applied,
        })
    }
}

// =============================================================================
// 4. TRUNCATION PIN BREAKER & TWO-WORD 64-PARTICIPANT CALVIN VOTE TABLE
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    None,
    FreezeCheckpoint { tenant: u32 },
    Relocate { tenant: u32 },
}

pub mod dirty;
pub use dirty::TruncationPins;

pub use calvin::{Participants, Vote, VoteTable};

// =============================================================================
// 5. SECURITY: KEYED PRF REGISTRY, $REF MACS, MACAROONS & CAMEL TAINT LATTICE
// =============================================================================

pub mod security;
pub use ciagomem_crypto::*;
pub use security::*;

// =============================================================================
// 6. LINUX SEALED 4 KiB AGENT EXPORT WINDOW (Fixes C4 & CVE-2024-57937 Gate)
// =============================================================================

#[cfg(target_os = "linux")]
pub mod export_window {
    use std::ffi::CString;
    use std::sync::{Arc, Mutex};
    pub struct ExportBudget {
        max_bytes: usize,
        max_sessions: usize,
        used: Mutex<(usize, usize)>,
    }
    impl ExportBudget {
        pub fn new(max_bytes: usize, max_sessions: usize) -> Arc<Self> {
            Arc::new(Self {
                max_bytes,
                max_sessions,
                used: Mutex::new((0, 0)),
            })
        }
        pub fn usage(&self) -> (usize, usize) {
            *self.used.lock().unwrap()
        }
        fn reserve(self: &Arc<Self>, len: usize) -> Result<Reservation, String> {
            let mut used = self
                .used
                .lock()
                .map_err(|_| "export budget lock poisoned")?;
            if used.1 >= self.max_sessions || len > self.max_bytes.saturating_sub(used.0) {
                return Err("export budget exhausted".into());
            }
            used.0 += len;
            used.1 += 1;
            Ok(Reservation {
                budget: self.clone(),
                len,
            })
        }
    }
    struct Reservation {
        budget: Arc<ExportBudget>,
        len: usize,
    }
    impl Drop for Reservation {
        fn drop(&mut self) {
            if let Ok(mut used) = self.budget.used.lock() {
                used.0 -= self.len;
                used.1 -= 1;
            }
        }
    }

    pub struct SealedExportWindow {
        pub engine_fd: i32,
        pub agent_ro_fd: i32,
        pub engine_ptr: *mut u8,
        pub len: usize,
        _reservation: Reservation,
    }

    // Raw writable mappings are intentionally not Send or Sync.

    impl SealedExportWindow {
        /// Creates a 4 KiB-aligned sealed export window, runs the CVE-2024-57937 mprotect
        /// self-test, and produces an O_RDONLY file descriptor safe for SCM_RIGHTS handoff.
        pub fn create_and_selftest(len: usize, budget: &Arc<ExportBudget>) -> Result<Self, String> {
            let reservation = budget.reserve(len)?;
            if len < 4096 || len % 4096 != 0 || len > isize::MAX as usize {
                return Err("invalid export window size".into());
            }
            unsafe {
                let name = CString::new("ciagomem-export").unwrap();
                let fd =
                    libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING);
                if fd < 0 {
                    return Err("memfd_create failed".into());
                }
                if libc::ftruncate(fd, len as libc::off_t) != 0 {
                    libc::close(fd);
                    return Err("ftruncate failed".into());
                }
                let ptr = libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    fd,
                    0,
                );
                if ptr == libc::MAP_FAILED {
                    libc::close(fd);
                    return Err("engine mmap failed".into());
                }
                let seals = libc::F_SEAL_SHRINK
                    | libc::F_SEAL_GROW
                    | libc::F_SEAL_FUTURE_WRITE
                    | libc::F_SEAL_SEAL;
                if libc::fcntl(fd, libc::F_ADD_SEALS, seals) != 0 {
                    libc::munmap(ptr, len);
                    libc::close(fd);
                    return Err("F_ADD_SEALS failed".into());
                }
                let proc_path = CString::new(format!("/proc/self/fd/{fd}")).unwrap();
                let ro_fd = libc::open(proc_path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
                if ro_fd < 0 {
                    libc::munmap(ptr, len);
                    libc::close(fd);
                    return Err("reopen O_RDONLY failed".into());
                }
                // Boot-time CVE-2024-57937 self-test: agent PROT_READ mapping MUST refuse mprotect(PROT_WRITE)
                let agent_map = libc::mmap(
                    core::ptr::null_mut(),
                    len,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    ro_fd,
                    0,
                );
                if agent_map == libc::MAP_FAILED {
                    libc::close(ro_fd);
                    libc::munmap(ptr, len);
                    libc::close(fd);
                    return Err("agent PROT_READ mmap failed".into());
                }
                let mprot = libc::mprotect(agent_map, len, libc::PROT_READ | libc::PROT_WRITE);
                libc::munmap(agent_map, len);
                if mprot == 0 {
                    libc::close(ro_fd);
                    libc::munmap(ptr, len);
                    libc::close(fd);
                    return Err("CVE-2024-57937 vulnerable kernel: mprotect(PROT_WRITE) succeeded on sealed RO fd".into());
                }
                Ok(SealedExportWindow {
                    engine_fd: fd,
                    agent_ro_fd: ro_fd,
                    engine_ptr: ptr as *mut u8,
                    len,
                    _reservation: reservation,
                })
            }
        }
    }

    impl Drop for SealedExportWindow {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.engine_ptr as *mut libc::c_void, self.len);
                libc::close(self.agent_ro_fd);
                libc::close(self.engine_fd);
            }
        }
    }
}

// =============================================================================

pub mod policy;
pub use coagomem_ql::{ParseError, Query as CiaQuery, Source, Stage, parse as parse_ciaql};
pub use policy::*;
pub mod calvin;
#[path = "../../coagomem-wal/src/envelope.rs"]
pub mod envelope;
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
