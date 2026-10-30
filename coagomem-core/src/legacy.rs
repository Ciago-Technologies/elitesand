//! CiagoMem - SLAE (Pre-Final v2.5) Complete Verified Core Implementation
//! Compiles and passes tests on Linux (x86_64 & aarch64), macOS, and Windows.

#![allow(clippy::missing_safety_doc)]

use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::sync::atomic::{
    AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst},
    fence,
};

// ============================================================================
// MODULE 1: PROT_READ-SAFE BOEHM FENCE SEQLOCK (`seqlock.rs`)
// ============================================================================

#[path = "legacy_hardening.rs"]
pub mod hardening;

pub const READ_RETRY_LIMIT: u32 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Writer held the lock for the whole retry budget, or the section is poisoned.
    WriterActive,
    /// The closure rejected a validated snapshot (layout corruption).
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterBusy;

/// Even = stable snapshot, Odd = write in progress (or poisoned if guard dropped).
#[derive(Debug, Default)]
#[repr(transparent)]
pub struct SeqWord(AtomicU64);

impl SeqWord {
    pub const fn new() -> Self {
        SeqWord(AtomicU64::new(0))
    }

    /// PROT_READ safe: issues only a Relaxed load followed by an Acquire fence [F1].
    #[inline(always)]
    pub fn read_begin(&self) -> Option<u64> {
        let s = self.0.load(Relaxed);
        fence(Acquire); // [F1]
        if (s & 1) == 0 { Some(s) } else { None }
    }

    /// PROT_READ safe: Acquire fence [F2] orders all prior Relaxed data loads before s2.
    #[inline(always)]
    pub fn read_retry(&self, s1: u64) -> bool {
        fence(Acquire); // [F2]
        self.0.load(Relaxed) != s1
    }

    #[inline(always)]
    pub fn write_begin(&self) -> Result<SeqWriteGuard<'_>, WriterBusy> {
        let s = self.0.load(Relaxed);
        if (s & 1) != 0 {
            return Err(WriterBusy);
        }
        if self.0.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
            return Err(WriterBusy);
        }
        fence(Release); // [W1]
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

#[must_use = "a write section must be committed or it poisons the slot"]
pub struct SeqWriteGuard<'a> {
    seq: &'a SeqWord,
    odd: u64,
    committed: bool,
}

impl SeqWriteGuard<'_> {
    #[inline(always)]
    pub fn commit(mut self) {
        self.committed = true;
        self.seq.0.store(self.odd + 1, Release); // [W2]
    }
}

impl Drop for SeqWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            debug_assert_eq!(self.seq.raw() & 1, 1);
        }
    }
}

/// Reader view: every accessor performs bounds-checked 8-byte Relaxed atomic loads.
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

    /// Copies arbitrary byte slices from 8-byte atomic loads (zero mixed-size atomic UB).
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

/// Exclusive writer view: mutates words strictly via 8-byte `AtomicU64::store(Relaxed)`.
pub struct WordsMut<'a> {
    words: &'a [AtomicU64],
}

impl<'a> WordsMut<'a> {
    #[inline(always)]
    pub fn new(words: &'a [AtomicU64]) -> Self {
        Self { words }
    }

    #[inline(always)]
    pub fn set(&mut self, i: usize, val: u64) {
        self.words[i].store(val, Relaxed);
    }

    #[inline]
    pub fn write_bytes(&mut self, byte_off: usize, src: &[u8]) {
        assert!(byte_off + src.len() <= self.words.len() * 8);
        let (mut inp, mut off) = (0, byte_off);
        while inp < src.len() {
            let idx = off / 8;
            let in_word = off % 8;
            let n = (8 - in_word).min(src.len() - inp);
            let mut w = if n == 8 {
                [0u8; 8]
            } else {
                self.words[idx].load(Relaxed).to_le_bytes()
            };
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

// ============================================================================
// MODULE 2: P x C SPSC MESH WITH 128B DOORBELLS & DEKKER WAKE (`mesh.rs`)
// ============================================================================

#[derive(Debug, Default)]
#[repr(C, align(128))]
pub struct CachePadded<T>(pub T);

impl<T> core::ops::Deref for CachePadded<T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &T {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    NotNeeded,
    Needed,
}

pub struct SpscLane<T, const N: usize> {
    tail: CachePadded<AtomicU64>,
    head: CachePadded<AtomicU64>,
    slots: Box<[UnsafeCell<MaybeUninit<T>>; N]>,
}

impl<T, const N: usize> SpscLane<T, N> {
    const ASSERT_POW2: () = assert!(N.is_power_of_two() && N >= 2);

    fn new() -> Self {
        let () = Self::ASSERT_POW2;
        let v: Vec<UnsafeCell<MaybeUninit<T>>> = (0..N)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect();
        let slots = v.into_boxed_slice().try_into().ok().unwrap();
        Self {
            tail: CachePadded(AtomicU64::new(0)),
            head: CachePadded(AtomicU64::new(0)),
            slots,
        }
    }

    #[inline(always)]
    fn slot(&self, seq: u64) -> *mut MaybeUninit<T> {
        self.slots[(seq as usize) & (N - 1)].get()
    }
}

impl<T, const N: usize> Drop for SpscLane<T, N> {
    fn drop(&mut self) {
        let mut h = self.head.load(Relaxed);
        let t = self.tail.load(Relaxed);
        while h != t {
            unsafe { (*self.slot(h)).assume_init_drop() };
            h += 1;
        }
    }
}

pub struct InboundBlock<T, const N: usize> {
    doorbell: Vec<CachePadded<AtomicU64>>,
    parked: CachePadded<AtomicU32>,
    rings: Vec<SpscLane<T, N>>,
}

unsafe impl<T: Send, const N: usize> Sync for InboundBlock<T, N> {}
unsafe impl<T: Send, const N: usize> Send for InboundBlock<T, N> {}

pub struct Producer<T, const N: usize> {
    block: std::sync::Arc<InboundBlock<T, N>>,
    src: usize,
    tail: u64,
    published: u64,
    cached_head: u64,
    _not_sync_or_clone: PhantomData<Cell<()>>,
}

unsafe impl<T: Send, const N: usize> Send for Producer<T, N> {}

pub struct Consumer<T, const N: usize> {
    block: std::sync::Arc<InboundBlock<T, N>>,
    heads: Vec<u64>,
    cached_tails: Vec<u64>,
    pending: Vec<u64>,
    _not_sync_or_clone: PhantomData<Cell<()>>,
}

unsafe impl<T: Send, const N: usize> Send for Consumer<T, N> {}

impl<T: Send, const N: usize> Producer<T, N> {
    #[inline(always)]
    fn ring(&self) -> &SpscLane<T, N> {
        &self.block.rings[self.src]
    }

    #[inline]
    pub fn free(&mut self) -> usize {
        let used = (self.tail - self.cached_head) as usize;
        if used < N {
            return N - used;
        }
        self.cached_head = self.ring().head.load(Acquire);
        N - (self.tail - self.cached_head) as usize
    }

    #[inline]
    pub fn try_push(&mut self, v: T) -> Result<(), T> {
        if self.tail - self.cached_head == N as u64 {
            self.cached_head = self.ring().head.load(Acquire);
            if self.tail - self.cached_head == N as u64 {
                return Err(v);
            }
        }
        unsafe { (*self.ring().slot(self.tail)).write(v) };
        self.tail += 1;
        Ok(())
    }

    #[inline]
    pub fn flush(&mut self) -> Wake {
        if self.tail == self.published {
            return Wake::NotNeeded;
        }
        self.ring().tail.store(self.tail, Release);
        self.published = self.tail;
        self.block.doorbell[self.src / 64].fetch_or(1u64 << (self.src % 64), AcqRel);
        fence(SeqCst); // Dekker barrier with prepare_park
        if self.block.parked.load(Relaxed) != 0 {
            Wake::Needed
        } else {
            Wake::NotNeeded
        }
    }
}

impl<T: Send, const N: usize> Consumer<T, N> {
    pub fn poll(&mut self, per_lane: usize, mut f: impl FnMut(usize, T)) -> usize {
        let mut delivered = 0;
        for w in 0..self.pending.len() {
            let mut bits = self.pending[w] | self.block.doorbell[w].swap(0, Acquire);
            let mut still = 0u64;
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let (n, more) = self.drain_lane(w * 64 + b, per_lane, &mut f);
                delivered += n;
                if more {
                    still |= 1u64 << b;
                }
            }
            self.pending[w] = still;
        }
        delivered
    }

    fn drain_lane(
        &mut self,
        src: usize,
        budget: usize,
        f: &mut impl FnMut(usize, T),
    ) -> (usize, bool) {
        let ring = &self.block.rings[src];
        let mut h = self.heads[src];
        if h == self.cached_tails[src] {
            self.cached_tails[src] = ring.tail.load(Acquire);
        }
        let t = self.cached_tails[src];
        let n = ((t - h) as usize).min(budget);
        for _ in 0..n {
            let v = unsafe { (*ring.slot(h)).assume_init_read() };
            h += 1;
            f(src, v);
        }
        if n > 0 {
            ring.head.store(h, Release);
        }
        self.heads[src] = h;
        (n, h != t)
    }

    pub fn prepare_park(&self) -> bool {
        if self.pending.iter().any(|&p| p != 0) {
            return false;
        }
        self.block.parked.store(1, Relaxed);
        fence(SeqCst); // Dekker barrier with flush
        let idle = self.block.doorbell.iter().all(|d| d.load(Relaxed) == 0);
        if !idle {
            self.block.parked.store(0, Relaxed);
        }
        idle
    }

    pub fn unpark(&self) {
        self.block.parked.store(0, Relaxed);
    }
}

pub fn build_mesh<T: Send, const N: usize>(
    producers: usize,
    consumers: usize,
) -> (Vec<Vec<Producer<T, N>>>, Vec<Consumer<T, N>>) {
    let words = producers.div_ceil(64);
    let blocks: Vec<std::sync::Arc<InboundBlock<T, N>>> = (0..consumers)
        .map(|_| {
            std::sync::Arc::new(InboundBlock {
                doorbell: (0..words).map(|_| CachePadded(AtomicU64::new(0))).collect(),
                parked: CachePadded(AtomicU32::new(0)),
                rings: (0..producers).map(|_| SpscLane::new()).collect(),
            })
        })
        .collect();

    let prods = (0..producers)
        .map(|src| {
            (0..consumers)
                .map(|dst| Producer {
                    block: blocks[dst].clone(),
                    src,
                    tail: 0,
                    published: 0,
                    cached_head: 0,
                    _not_sync_or_clone: PhantomData,
                })
                .collect()
        })
        .collect();

    let cons = blocks
        .into_iter()
        .map(|block| Consumer {
            heads: vec![0; producers],
            cached_tails: vec![0; producers],
            pending: vec![0; words],
            block,
            _not_sync_or_clone: PhantomData,
        })
        .collect();

    (prods, cons)
}

pub fn admit_all_or_nothing<T: Send + Clone, const N: usize>(
    lanes: &mut [Producer<T, N>],
    targets: &[usize],
    msg: T,
) -> Result<usize, ()> {
    for &c in targets {
        if lanes[c].free() == 0 {
            return Err(());
        }
    }
    let mut wakes = 0;
    for &c in targets {
        let _ = lanes[c].try_push(msg.clone());
        if lanes[c].flush() == Wake::Needed {
            wakes += 1;
        }
    }
    Ok(wakes)
}

// ============================================================================
// MODULE 3: SLOT VALIDATION, 40-BIT GENERATIONS & ROUTE TABLE (`slot.rs`)
// ============================================================================

pub const IDX_BITS: u32 = 20;
pub const CLASS_BITS: u32 = 4;
pub const GEN_BITS: u32 = 64 - IDX_BITS - CLASS_BITS; // 40
pub const MAX_INDEX: u32 = (1 << IDX_BITS) - 1;
pub const MAX_CLASS: u8 = (1 << CLASS_BITS) - 1;
pub const MAX_GENERATION: u64 = (1 << GEN_BITS) - 1;
const IDX_MASK: u64 = (1 << IDX_BITS) - 1;
const CLASS_SHIFT: u32 = IDX_BITS;
const CLASS_MASK: u64 = ((1 << CLASS_BITS) - 1) << CLASS_SHIFT;
const GEN_SHIFT: u32 = IDX_BITS + CLASS_BITS;

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
            (generation << GEN_SHIFT) | ((class as u64) << CLASS_SHIFT) | index as u64,
        ))
    }

    #[inline(always)]
    pub const fn from_raw(raw: u64) -> Self {
        SlotHandle(raw)
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
    pub const fn is_null(self) -> bool {
        self.0 == 0
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
        Self {
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
    #[inline(always)]
    pub const fn new(container: u16, block_4k: u64, copy_b: bool) -> Self {
        ExtentLoc(
            (block_4k & ((1 << 48) - 1))
                | ((copy_b as u64) << 51)
                | (((container & 0x0FFF) as u64) << 52),
        )
    }

    #[inline(always)]
    pub const fn container(self) -> u16 {
        (self.0 >> 52) as u16
    }

    #[inline(always)]
    pub const fn copy_b(self) -> bool {
        ((self.0 >> 51) & 1) != 0
    }

    #[inline(always)]
    pub const fn base_block_4k(self) -> u64 {
        self.0 & ((1 << 48) - 1)
    }

    #[inline(always)]
    pub const fn current_offset(self, extent_bytes: u64) -> u64 {
        self.base_block_4k() * 4096 + if self.copy_b() { extent_bytes } else { 0 }
    }

    #[inline(always)]
    pub const fn flipped(self) -> Self {
        ExtentLoc(self.0 ^ (1u64 << 51))
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
    pub reserved: AtomicU64,
}
const _: () = assert!(core::mem::size_of::<SlotHeader>() == 64);

#[repr(C, align(64))]
pub struct CoreControl {
    durable_lsn: AtomicU64,
    _pad: [u64; 7],
}

impl CoreControl {
    pub const fn new() -> Self {
        Self {
            durable_lsn: AtomicU64::new(0),
            _pad: [0; 7],
        }
    }

    /// PROT_READ safe durable watermark load.
    #[inline(always)]
    pub fn durable(&self) -> u64 {
        let d = self.durable_lsn.load(Relaxed);
        fence(Acquire);
        d
    }

    #[inline(always)]
    pub fn publish_durable(&self, lsn: u64) {
        self.durable_lsn.store(lsn, Release);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Optimistic,
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot<T> {
    pub value: T,
    pub applied_lsn: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotReadError {
    InvalidHandle,
    Reassigned,
    WriterActive,
    NotYetDurable { applied: u64, durable: u64 },
    Corrupt,
}

#[repr(C, align(4096))]
pub struct AlignedPayload<const W: usize>(pub [AtomicU64; W]);

pub struct SlotArena<const W: usize> {
    class: u8,
    headers: Box<[SlotHeader]>,
    payloads: Box<[AlignedPayload<W>]>,
    cores: Box<[CoreControl]>,
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
                reserved: AtomicU64::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();

        let payloads = (0..slots)
            .map(|_| AlignedPayload(core::array::from_fn(|_| AtomicU64::new(0))))
            .collect::<Vec<_>>()
            .into_boxed_slice();

        let cores = (0..num_cores)
            .map(|_| CoreControl::new())
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self {
            class,
            headers,
            payloads,
            cores,
        }
    }

    #[inline(always)]
    pub fn core_control(&self, core: usize) -> &CoreControl {
        &self.cores[core]
    }

    #[inline(always)]
    pub fn resolve(&self, h: SlotHandle) -> Option<u32> {
        if h.is_null() || h.class() != self.class || (h.index() as usize) >= self.headers.len() {
            None
        } else {
            Some(h.index())
        }
    }

    #[inline(always)]
    pub fn header(&self, idx: u32) -> &SlotHeader {
        &self.headers[idx as usize]
    }

    #[inline(always)]
    pub fn payload(&self, idx: u32) -> &[AtomicU64; W] {
        &self.payloads[idx as usize].0
    }

    #[inline]
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
        let r = read_with(&hdr.seq, words, READ_RETRY_LIMIT, |view| {
            let owner = hdr.owner.load(Relaxed);
            let tw = TenantWord::unpack(hdr.tenant.load(Relaxed));
            if owner != h.raw() || tw.tenant != tenant {
                return Some(Err(()));
            }
            let applied = hdr.applied_lsn.load(Relaxed);
            Some(Ok((f(view)?, applied, tw.core)))
        })
        .map_err(|e| match e {
            ReadError::WriterActive => SlotReadError::WriterActive,
            ReadError::Corrupt => SlotReadError::Corrupt,
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

    pub fn install(
        &self,
        idx: u32,
        tw: TenantWord,
        applied_lsn: u64,
        checkpoint_lsn: u64,
    ) -> Result<Option<SlotHandle>, WriterBusy> {
        let hdr = self.header(idx);
        let last = hdr.last_generation.load(Relaxed);
        let Some(h) = SlotHandle::pack(idx, self.class, last + 1) else {
            return Ok(None);
        };
        let g = hdr.seq.write_begin()?;
        debug_assert_eq!(hdr.owner.load(Relaxed), 0);
        hdr.last_generation.store(h.generation(), Relaxed);
        hdr.tenant.store(tw.pack(), Relaxed);
        hdr.applied_lsn.store(applied_lsn, Relaxed);
        hdr.checkpoint_lsn.store(checkpoint_lsn, Relaxed);
        hdr.payload_len.store(Self::PAYLOAD_BYTES as u64, Relaxed);
        hdr.owner.store(h.raw(), Relaxed);
        g.commit();
        Ok(Some(h))
    }

    pub fn write(
        &self,
        h: SlotHandle,
        new_lsn: u64,
        f: impl FnOnce(&mut WordsMut<'_>),
    ) -> Result<(), WriterBusy> {
        let idx = self.resolve(h).expect("invalid handle on owner core");
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        debug_assert_eq!(hdr.owner.load(Relaxed), h.raw());
        let mut wm = WordsMut::new(self.payload(idx));
        f(&mut wm);
        if new_lsn > hdr.applied_lsn.load(Relaxed) {
            hdr.applied_lsn.store(new_lsn, Relaxed);
        }
        g.commit();
        Ok(())
    }

    pub fn revoke(&self, h: SlotHandle) -> Result<(), WriterBusy> {
        let idx = self.resolve(h).expect("invalid handle on owner core");
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        debug_assert_eq!(hdr.owner.load(Relaxed), h.raw());
        hdr.owner.store(0, Relaxed);
        hdr.tenant.store(0, Relaxed);
        g.commit();
        Ok(())
    }
}

#[repr(C, align(16))]
pub struct RouteEntry {
    pub handle: AtomicU64,
    pub extent: AtomicU64,
}
const _: () = assert!(core::mem::size_of::<RouteEntry>() == 16);

// ============================================================================
// MODULE 4: TWO-WORD TAGGED CALVIN VOTE TABLE (`calvin.rs`)
// ============================================================================

pub const MAX_PARTICIPANTS: u32 = 64;
const EMPTY_TAG: u64 = 0;

#[inline(always)]
pub const fn full_mask(n: u32) -> Option<u64> {
    if n == 0 || n > MAX_PARTICIPANTS {
        None
    } else {
        Some(u64::MAX >> (64 - n))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Participants(u8);

impl Participants {
    #[inline(always)]
    pub const fn new(n: u32) -> Option<Participants> {
        if n == 0 || n > MAX_PARTICIPANTS {
            None
        } else {
            Some(Participants(n as u8))
        }
    }

    #[inline(always)]
    pub const fn mask(self) -> u64 {
        u64::MAX >> (64 - self.0 as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    Pending,
    Decided { commit: bool },
    Duplicate,
    Stale,
}

#[repr(C, align(64))]
struct TxSlot {
    tag: AtomicU64,
    mask: AtomicU64,
    voted: AtomicU64,
    nays: AtomicU64,
}

pub struct VoteTable {
    slots: Box<[TxSlot]>,
    slot_mask: usize,
}

impl VoteTable {
    pub fn new(capacity_pow2: usize) -> Self {
        assert!(capacity_pow2.is_power_of_two());
        let slots = (0..capacity_pow2)
            .map(|_| TxSlot {
                tag: AtomicU64::new(EMPTY_TAG),
                mask: AtomicU64::new(0),
                voted: AtomicU64::new(0),
                nays: AtomicU64::new(0),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            slot_mask: capacity_pow2 - 1,
        }
    }

    #[inline(always)]
    fn slot(&self, tx_seq: u64) -> &TxSlot {
        &self.slots[(tx_seq as usize) & self.slot_mask]
    }

    pub fn open(&self, tx_seq: u64, n: Participants) -> Result<(), &'static str> {
        assert_ne!(tx_seq, EMPTY_TAG);
        let s = self.slot(tx_seq);
        if s.tag.load(Acquire) != EMPTY_TAG {
            return Err("Calvin TxSlot busy");
        }
        s.mask.store(n.mask(), Relaxed);
        s.voted.store(0, Relaxed);
        s.nays.store(0, Relaxed);
        s.tag.store(tx_seq, Release);
        Ok(())
    }

    pub fn vote(&self, tx_seq: u64, idx: u32, ok: bool) -> Vote {
        let s = self.slot(tx_seq);
        if s.tag.load(Acquire) != tx_seq {
            return Vote::Stale;
        }
        let mask = s.mask.load(Relaxed);
        if idx >= MAX_PARTICIPANTS || ((mask >> idx) & 1) == 0 {
            return Vote::Stale;
        }
        let bit = 1u64 << idx;
        if !ok {
            s.nays.fetch_or(bit, Release); // Nay recorded BEFORE vote bit
        }
        let before = s.voted.fetch_or(bit, AcqRel);
        if (before & bit) != 0 {
            return Vote::Duplicate;
        }
        if ((before | bit) & mask) == mask {
            let commit = (s.nays.load(Acquire) & mask) == 0;
            Vote::Decided { commit }
        } else {
            Vote::Pending
        }
    }

    pub fn close(&self, tx_seq: u64) {
        let _ = self
            .slot(tx_seq)
            .tag
            .compare_exchange(tx_seq, EMPTY_TAG, AcqRel, Relaxed);
    }
}

// ============================================================================
// MODULE 5: CIAQL ( & COAGOQL) SPAN-VALIDATED PARSER (`ciaql.rs`)
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub at: usize,
    pub end: usize,
    pub msg: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source<'a> {
    Actor(u64),
    Alias(&'a str),
    TxActors(Vec<u64>),
    ScanAll,
    ScanRange { start: u64, end: u64 },
    ResolveTenant { name: &'a str, alias: &'a str },
    BackupStore { destination: &'a str },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage<'a> {
    Filter(&'a str),
    Mutate(&'a str),
    Project(Vec<&'a str>),
    GroupBy(Vec<&'a str>),
    Aggregate(Vec<&'a str>),
    Similarity {
        threshold: f32,
    },
    SearchBm25 {
        query: &'a str,
        top_k: u32,
    },
    JoinActor {
        target_expr: &'a str,
        alias: &'a str,
    },
    CheckpointFrame {
        intent: &'a str,
    },
    HydrateFrame {
        frame_hash: u64,
    },
    ExportTo {
        format: &'a str,
        destination: &'a str,
    },
    Take(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CiaQuery<'a> {
    pub source: Source<'a>,
    pub stages: Vec<Stage<'a>>,
}

/// Parses both ergonomic CiaQL (`@0x0008F421`, `resolve tenant`, `backup store`, `join`, `search`)
/// and explicit `|>` pipelines, rejecting any unknown stage keyword with a hard error.
pub fn parse_ciaql<'a>(src: &'a str) -> Result<CiaQuery<'a>, ParseError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err(ParseError {
            at: 0,
            end: 0,
            msg: "empty CiaQL query",
        });
    }

    // Check top-level utility commands first
    if let Some(rest) = trimmed.strip_prefix("resolve tenant ") {
        let parts: Vec<&str> = rest.split(" as ").map(str::trim).collect();
        if parts.len() == 2 {
            return Ok(CiaQuery {
                source: Source::ResolveTenant {
                    name: parts[0].trim_matches('"'),
                    alias: parts[1],
                },
                stages: Vec::new(),
            });
        }
    }
    if let Some(rest) = trimmed.strip_prefix("backup store to ") {
        let dest = rest
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches('"');
        return Ok(CiaQuery {
            source: Source::BackupStore { destination: dest },
            stages: Vec::new(),
        });
    }

    // Split pipeline segments safely outside quotes and nested delimiters
    let segments = split_ciaql_segments(trimmed)?;
    let first = segments[0];

    let source = if let Some(hex_or_alias) = first.strip_prefix('@') {
        let token = hex_or_alias.split_whitespace().next().unwrap_or("");
        if token.starts_with("0x")
            || token.starts_with("0X")
            || token.chars().all(|c| c.is_ascii_digit())
        {
            Source::Actor(parse_u64_clean(token)?)
        } else {
            Source::Alias(token)
        }
    } else if let Some(inner) = first
        .strip_prefix("ACTOR(")
        .and_then(|s| s.strip_suffix(')'))
    {
        Source::Actor(parse_u64_clean(inner)?)
    } else if let Some(inner) = first
        .strip_prefix("TX_ACTORS[")
        .and_then(|s| s.strip_suffix(']'))
    {
        let mut ids = Vec::new();
        for p in inner.split(',') {
            ids.push(parse_u64_clean(p)?);
        }
        if ids.len() < 2 || ids.len() > 64 {
            return Err(ParseError {
                at: 0,
                end: first.len(),
                msg: "TX_ACTORS requires 2..=64 participants",
            });
        }
        Source::TxActors(ids)
    } else if first.starts_with("SCAN ACTORS[*]")
        || first.starts_with("scan actors[*]")
        || first.starts_with("scan users[*]")
    {
        Source::ScanAll
    } else if let Some(r) = first
        .strip_prefix("SCAN ACTORS[")
        .and_then(|s| s.strip_suffix(']'))
    {
        let (a, b) = r.split_once("..").ok_or(ParseError {
            at: 0,
            end: first.len(),
            msg: "invalid SCAN range",
        })?;
        Source::ScanRange {
            start: parse_u64_clean(a)?,
            end: parse_u64_clean(b)?,
        }
    } else {
        return Err(ParseError {
            at: 0,
            end: first.len(),
            msg: "unrecognized CiaQL source clause",
        });
    };

    let mut stages = Vec::new();
    for seg in &segments[1..] {
        let kw_end = seg
            .find(|c: char| c.is_whitespace() || c == '(' || c == '{')
            .unwrap_or(seg.len());
        let kw = &seg[..kw_end];
        match kw {
            "FILTER" | "where" => stages.push(Stage::Filter(seg[kw_end..].trim())),
            "MUTATE" | "mutate" => stages.push(Stage::Mutate(seg[kw_end..].trim())),
            "PROJECT" | "select" | "project" => {
                stages.push(Stage::Project(vec![seg[kw_end..].trim()]))
            }
            "GROUP_BY" | "group" => stages.push(Stage::GroupBy(vec![seg[kw_end..].trim()])),
            "AGGREGATE" | "REDUCE" | "aggregate" => {
                stages.push(Stage::Aggregate(vec![seg[kw_end..].trim()]))
            }
            "SIMILARITY" | "similarity" => stages.push(Stage::Similarity { threshold: 0.80 }),
            "SEARCH" | "search" => stages.push(Stage::SearchBm25 {
                query: seg[kw_end..].trim(),
                top_k: 10,
            }),
            "JOIN" | "join" => stages.push(Stage::JoinActor {
                target_expr: seg[kw_end..].trim(),
                alias: "@joined",
            }),
            "CHECKPOINT_FRAME" | "checkpoint" => stages.push(Stage::CheckpointFrame {
                intent: seg[kw_end..].trim(),
            }),
            "HYDRATE_FRAME" | "hydrate" => {
                let inner = seg[kw_end..]
                    .trim()
                    .trim_start_matches('(')
                    .trim_end_matches(')');
                stages.push(Stage::HydrateFrame {
                    frame_hash: parse_u64_clean(inner)?,
                });
            }
            "EXPORT_TO" | "export" => stages.push(Stage::ExportTo {
                format: "PARQUET",
                destination: seg[kw_end..].trim(),
            }),
            "TAKE" | "take" | "limit" => {
                let n = parse_u64_clean(seg[kw_end..].trim())?;
                if n == 0 {
                    return Err(ParseError {
                        at: 0,
                        end: seg.len(),
                        msg: "TAKE/limit must be >= 1",
                    });
                }
                stages.push(Stage::Take(n));
            }
            _ => {
                return Err(ParseError {
                    at: 0,
                    end: seg.len(),
                    msg: "unknown stage keyword",
                });
            }
        }
    }

    Ok(CiaQuery { source, stages })
}

fn split_ciaql_segments<'a>(src: &'a str) -> Result<Vec<&'a str>, ParseError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut in_str = false;
    let mut depth = 0i32;

    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'(' | b'{' | b'[' => depth += 1,
            b')' | b'}' | b']' => {
                depth -= 1;
                if depth < 0 {
                    return Err(ParseError {
                        at: i,
                        end: i + 1,
                        msg: "unbalanced closing delimiter",
                    });
                }
            }
            b'|' if depth == 0 && i + 1 < bytes.len() && bytes[i + 1] == b'>' => {
                out.push(src[start..i].trim());
                i += 2;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if in_str || depth != 0 {
        return Err(ParseError {
            at: 0,
            end: src.len(),
            msg: "unterminated string or delimiter block",
        });
    }
    out.push(src[start..].trim());
    Ok(out)
}

fn parse_u64_clean(raw: &str) -> Result<u64, ParseError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(ParseError {
            at: 0,
            end: 0,
            msg: "empty integer literal",
        });
    }
    let (radix, digits) = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16u32, h.as_bytes())
    } else {
        (10u32, s.as_bytes())
    };
    let mut acc = 0u64;
    let mut saw = false;
    for &b in digits {
        if b == b'_' {
            continue;
        }
        let d = (b as char).to_digit(radix).ok_or(ParseError {
            at: 0,
            end: s.len(),
            msg: "invalid integer digit",
        })? as u64;
        acc = acc
            .checked_mul(radix as u64)
            .and_then(|v| v.checked_add(d))
            .ok_or(ParseError {
                at: 0,
                end: s.len(),
                msg: "integer literal overflow",
            })?;
        saw = true;
    }
    if saw {
        Ok(acc)
    } else {
        Err(ParseError {
            at: 0,
            end: s.len(),
            msg: "invalid integer literal",
        })
    }
}

// ============================================================================
// MODULE 6: VERIFICATION UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prot_read_safe_seqlock_and_generational_slot_validation() {
        let arena = SlotArena::<16>::new(0, 8, 2);
        let tw1 = TenantWord {
            tenant: 101,
            core: 0,
            layout_version: 1,
            flags: 0,
        };
        let h1 = arena.install(3, tw1, 10, 10).unwrap().unwrap();
        arena
            .write(h1, 11, |wm| {
                wm.set(0, 0xCAFE_BABE);
                wm.write_bytes(8, b"CiagoMem");
            })
            .unwrap();

        // Durable read fails before CoreControl::publish_durable(11)
        assert_eq!(
            arena.read(h1, 101, Visibility::Durable, |v| v.word(0)),
            Err(SlotReadError::NotYetDurable {
                applied: 11,
                durable: 0
            })
        );

        // Publish durable watermark on core 0
        arena.core_control(0).publish_durable(11);
        let snap = arena
            .read(h1, 101, Visibility::Durable, |v| {
                let mut b = [0u8; 8];
                v.copy_bytes(8, &mut b)?;
                Some((v.word(0)?, b))
            })
            .unwrap();
        assert_eq!(snap.value.0, 0xCAFE_BABE);
        assert_eq!(&snap.value.1, b"CiagoMem");

        // Evict tenant 101 and install tenant 202 in the same physical slot 3
        arena.revoke(h1).unwrap();
        let tw2 = TenantWord {
            tenant: 202,
            core: 0,
            layout_version: 1,
            flags: 0,
        };
        let h2 = arena.install(3, tw2, 1, 1).unwrap().unwrap();
        assert_eq!(h2.generation(), h1.generation() + 1);

        // Old handle h1 is deterministically rejected
        assert_eq!(
            arena.read(h1, 101, Visibility::Optimistic, |v| v.word(0)),
            Err(SlotReadError::Reassigned)
        );
    }

    #[test]
    fn spsc_mesh_doorbell_and_all_or_nothing_admission() {
        let (mut prods, mut cons) = build_mesh::<u64, 8>(3, 2);
        assert!(cons[1].prepare_park());
        let wakes = admit_all_or_nothing(&mut prods[0], &[0, 1], 777).unwrap();
        assert_eq!(wakes, 1); // Consumer 1 was parked so it requested a wake CQE

        let mut received = Vec::new();
        cons[1].poll(8, |src, msg| received.push((src, msg)));
        assert_eq!(received, vec![(0, 777)]);
    }

    #[test]
    fn calvin_64_participants_and_unique_decider() {
        let vt = VoteTable::new(16);
        let p64 = Participants::new(64).unwrap();
        assert_eq!(p64.mask(), u64::MAX);
        vt.open(99, p64).unwrap();

        for idx in 0..63 {
            assert_eq!(vt.vote(99, idx, true), Vote::Pending);
        }
        // Participant 63 votes YES: must not collide with an abort bit!
        assert_eq!(vt.vote(99, 63, true), Vote::Decided { commit: true });
        vt.close(99);
    }

    #[test]
    fn ciaql_parses_shorthand_pipelines_and_rejects_unknown_stages() {
        let q1 = parse_ciaql(
            r#"@0x0008F421 |> where (state.msg == "a |> b") |> mutate { x = 1 } |> checkpoint(intent: "handoff") |> take 5"#,
        )
        .unwrap();
        assert_eq!(q1.source, Source::Actor(0x0008F421));
        assert_eq!(q1.stages.len(), 4);

        let q2 = parse_ciaql(r#"resolve tenant "acme_corp" as @acme"#).unwrap();
        assert_eq!(
            q2.source,
            Source::ResolveTenant {
                name: "acme_corp",
                alias: "@acme"
            }
        );

        let bad = parse_ciaql(r#"@0x0008F421 |> MUTTATE { x = 1 }"#);
        assert!(bad.is_err());
    }
}
