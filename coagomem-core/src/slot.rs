//! Generational slot handles, the one-cache-line slot header, the slab arena and the
//! tenant route table.
//!
//! Spec defect: `TenantRouteEntry.slab_slot_index: u32` is a bare index. After eviction
//! and reuse, a reader holding the old index validates the *new* tenant's SeqLock and
//! returns another tenant's state (cross-tenant read).
//!
//! Fix: every reference to a slot is a 64-bit `SlotHandle`
//!
//! ```text
//!  63                                   24 23    20 19                    0
//! +---------------------------------------+--------+----------------------+
//! |          generation (40 bits)         | class  |  index in class (20) |
//! +---------------------------------------+--------+----------------------+
//! ```
//!
//! * `generation` starts at 1 and is bumped on every reassignment; raw value 0 is
//!   `SlotHandle::NULL`, so a zero-filled route table means "everything cold".
//!   2^40 reuses of one slot at 10k reuses/s per slot is 3.5 years of *continuous*
//!   churn on that one slot; on exhaustion the slot is retired, never wrapped.
//! * `class` selects one of 16 slab size classes (4 KiB .. 64 KiB extents).
//! * `index` addresses 2^20 slots per class.
//!
//! The slot header stores the exact `SlotHandle` of its current owner in the same
//! 64-byte line as the sequence word, so validating "is this still my tenant's slot"
//! costs one `cmp` on a line the reader already loaded for the SeqLock: zero extra
//! cache misses, header still exactly one line.

use crate::CachePadded;
use crate::seqlock::{ReadError, SeqWord, WordView, WordsMut, WriterBusy, read_deadline};
use core::ptr::NonNull;
use core::sync::atomic::{
    AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
    fence,
};

// ---------------------------------------------------------------------------------
// SlotHandle
// ---------------------------------------------------------------------------------

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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct SlotHandle(u64);

impl core::fmt::Debug for SlotHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_null() {
            return f.write_str("SlotHandle(NULL)");
        }
        write!(
            f,
            "SlotHandle(gen={}, class={}, idx={})",
            self.generation(),
            self.class(),
            self.index()
        )
    }
}

impl SlotHandle {
    pub const NULL: SlotHandle = SlotHandle(0);

    /// `None` if any field is out of range or `generation == 0`.
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
    #[inline(always)]
    pub const fn from_raw(raw: u64) -> SlotHandle {
        SlotHandle(raw)
    }
    /// Handle for the next owner of the same physical slot; `None` = retire the slot.
    #[inline(always)]
    pub const fn successor(self) -> Option<SlotHandle> {
        SlotHandle::pack(self.index(), self.class(), self.generation() + 1)
    }
}

// ---------------------------------------------------------------------------------
// Tenant word, extent location
// ---------------------------------------------------------------------------------

/// `[ dense_tenant:32 | owner_core:8 | layout_version:8 | flags:16 ]`
/// Dense u32 tenant ids (assigned at tenant creation) index the route table directly;
/// external 64-bit ids are mapped once at the protocol edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            | (self.core as u64) << 32
            | (self.layout_version as u64) << 40
            | (self.flags as u64) << 48
    }
    #[inline(always)]
    pub const fn unpack(w: u64) -> TenantWord {
        TenantWord {
            tenant: w as u32,
            core: (w >> 32) as u8,
            layout_version: (w >> 40) as u8,
            flags: (w >> 48) as u16,
        }
    }
}

/// `[ container:12 | copy:1 | flags:3 | block_4k:48 ]`
/// Each extent owns two on-disk copies (A at `block_4k`, B at `block_4k + span`).
/// Checkpoints always target the copy that is *not* current, so a torn checkpoint
/// write can never destroy the last good image (NVMe only guarantees power-fail
/// atomicity up to AWUPF, typically one LBA).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ExtentLoc(pub u64);

impl ExtentLoc {
    pub const NONE: ExtentLoc = ExtentLoc(0);
    const BLOCK_MASK: u64 = (1 << 48) - 1;
    pub const fn new(container: u16, block_4k: u64, copy_b: bool) -> ExtentLoc {
        assert!(container < 4096 && block_4k <= Self::BLOCK_MASK);
        ExtentLoc((container as u64) << 52 | (copy_b as u64) << 51 | block_4k)
    }
    pub const fn container(self) -> u16 {
        (self.0 >> 52) as u16
    }
    pub const fn copy_b(self) -> bool {
        (self.0 >> 51) & 1 == 1
    }
    pub const fn base_block(self) -> u64 {
        self.0 & Self::BLOCK_MASK
    }
    /// Byte offset of the currently valid copy.
    pub const fn current_offset(self, extent_bytes: u64) -> u64 {
        self.base_block() * 4096 + if self.copy_b() { extent_bytes } else { 0 }
    }
    /// The other copy: the target of the next checkpoint.
    pub const fn flipped(self) -> ExtentLoc {
        ExtentLoc(self.0 ^ (1 << 51))
    }
}

// ---------------------------------------------------------------------------------
// Slot header (exactly one cache line) and per-core control line
// ---------------------------------------------------------------------------------

#[derive(Debug, Default)]
#[repr(C, align(64))]
pub struct SlotHeader {
    /// SeqLock word protecting this header *and* the slot payload.
    pub seq: SeqWord, //                         +0
    /// `SlotHandle::raw()` of the current owner; 0 while free / loading / evicting.
    pub owner: AtomicU64, //                     +8
    /// `TenantWord::pack()`.
    pub tenant: AtomicU64, //                    +16
    /// Core-local LSN of the last record applied to the payload.
    pub applied_lsn: AtomicU64, //               +24
    /// LSN contained in the on-disk image (informational for readers).
    pub checkpoint_lsn: AtomicU64, //            +32
    /// Valid payload bytes.
    pub payload_len: AtomicU64, //               +40
    /// Last generation issued for this physical slot (engine-private bookkeeping
    /// kept here so it survives the slot being free).
    pub last_generation: AtomicU64, //           +48
    pub reserved: AtomicU64, //                  +56
}
const _: () = assert!(core::mem::size_of::<SlotHeader>() == 64);

/// Per-core durability watermark, exported read-only to agents.
#[derive(Debug, Default)]
#[repr(C)]
pub struct CoreControl {
    /// Highest LSN of this core's log with a successful CQE for it and every prior record.
    pub durable_lsn: CachePadded<AtomicU64>,
    /// Bumped on every engine (re)start; readers drop all cached handles on change.
    pub engine_epoch: CachePadded<AtomicU64>,
}

impl CoreControl {
    /// Reader side: relaxed load + acquire fence (read-only-mapping safe).
    #[inline(always)]
    pub fn durable(&self) -> u64 {
        let d = self.durable_lsn.load(Relaxed);
        fence(Acquire);
        d
    }
    /// Log side: monotonic publish.
    #[inline(always)]
    pub fn publish_durable(&self, lsn: u64) {
        self.durable_lsn.fetch_max(lsn, Release);
    }
}

// ---------------------------------------------------------------------------------
// Payload and arena
// ---------------------------------------------------------------------------------

/// One slot's payload. 4 KiB alignment makes it a valid O_DIRECT / `WRITE_FIXED`
/// source and destination with no bounce buffer. Words 0..8 hold the on-disk extent
/// image header (`envelope::ExtentImage`), application data starts at word 8.
#[derive(Debug)]
#[repr(C, align(4096))]
pub struct Payload<const W: usize>(pub [AtomicU64; W]);

pub const IMAGE_HEADER_WORDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Latest applied state (may not survive a crash). Read-your-writes for the engine.
    Applied,
    /// Only state whose log records are durable. Required for anything that leaves the
    /// node (agent handoff, external side effects).
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotReadError {
    /// The handle no longer owns the slot: re-resolve via the route table.
    Reassigned,
    /// Snapshot is newer than the durability watermark. `applied - durable` is bounded
    /// by one group-commit batch; retry or go through the owner core's mailbox.
    NotYetDurable {
        applied: u64,
        durable: u64,
    },
    WriterActive,
    WriterStalled,
    DeadlineExhausted,
    Corrupt,
    /// Handle is NULL or out of range for this arena.
    InvalidHandle,
}

impl From<ReadError> for SlotReadError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::WriterActive => SlotReadError::WriterActive,
            ReadError::Corrupt => SlotReadError::Corrupt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot<R> {
    pub value: R,
    pub applied_lsn: u64,
}

enum Backing {
    Heap {
        layout: std::alloc::Layout,
    },
    #[cfg(target_os = "linux")]
    Mmap {
        len: usize,
        _fd: std::os::fd::OwnedFd,
    },
}

/// One size class of the slab: a dense header array plus 4 KiB-aligned payloads.
/// Production backing is a `memfd_create(MFD_HUGETLB | MFD_HUGE_2MB)` mapping (see
/// `SlotArena::new_hugetlb`), so agents can map the very same pages read-only.
pub struct SlotArena<const W: usize> {
    class: u8,
    n: u32,
    headers: NonNull<SlotHeader>,
    payloads: NonNull<Payload<W>>,
    cores: Box<[CoreControl]>,
    backing: Backing,
    base: NonNull<u8>,
}

impl<const W: usize> core::fmt::Debug for SlotArena<W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotArena")
            .field("class", &self.class)
            .field("slots", &self.n)
            .field("payload_bytes", &(W * 8))
            .finish()
    }
}

// SAFETY: all shared state is atomics; raw pointers point into memory owned by `backing`.
unsafe impl<const W: usize> Send for SlotArena<W> {}
unsafe impl<const W: usize> Sync for SlotArena<W> {}

impl<const W: usize> SlotArena<W> {
    pub const PAYLOAD_BYTES: usize = W * 8;

    const fn header_region(n: usize) -> usize {
        (n * 64).next_multiple_of(2 * 1024 * 1024)
    }

    /// Bytes of backing memory needed for `n` slots (headers rounded to 2 MiB).
    pub const fn region_bytes(n: usize) -> usize {
        Self::header_region(n) + n * core::mem::size_of::<Payload<W>>()
    }

    fn check(class: u8, n: u32, cores: usize) {
        const { assert!(W * 8 >= 4096 && (W * 8) % 4096 == 0 && W > IMAGE_HEADER_WORDS) };
        assert!(class <= MAX_CLASS && n >= 1 && n - 1 <= MAX_INDEX && (1..=256).contains(&cores));
    }

    /// Zeroed heap backing (tests, non-Linux, or no hugepages reserved).
    pub fn new_heap(class: u8, n: u32, cores: usize) -> Self {
        Self::check(class, n, cores);
        let layout =
            std::alloc::Layout::from_size_align(Self::region_bytes(n as usize), 2 * 1024 * 1024)
                .unwrap();
        // SAFETY: non-zero size; zero bytes are a valid bit pattern for every atomic here.
        let base = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        Self::from_base(class, n, cores, base, Backing::Heap { layout })
    }

    /// HugeTLB-backed memfd (2 MiB pages, reserved at boot via `hugepages=`), mapped
    /// `MAP_SHARED | MAP_POPULATE`. Returns the fd too so it can be passed to agent
    /// processes over SCM_RIGHTS for a read-only mapping.
    #[cfg(target_os = "linux")]
    pub fn new_hugetlb(
        class: u8,
        n: u32,
        cores: usize,
    ) -> std::io::Result<(Self, std::os::fd::RawFd)> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        Self::check(class, n, cores);
        let len = Self::region_bytes(n as usize).next_multiple_of(2 * 1024 * 1024);
        let name = c"coagomem-slab";
        // MFD_HUGE_2MB = 21 << MFD_HUGE_SHIFT(26)
        let flags = libc::MFD_CLOEXEC | libc::MFD_HUGETLB | (21 << 26) | libc::MFD_ALLOW_SEALING;
        // SAFETY: plain syscalls with checked results.
        unsafe {
            let fd = libc::memfd_create(name.as_ptr(), flags);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            if libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Size is now fixed for the agents' mappings.
            libc::fcntl(
                fd.as_raw_fd(),
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            );
            let p = libc::mmap(
                core::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_POPULATE,
                fd.as_raw_fd(),
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            let raw = fd.as_raw_fd();
            let base = NonNull::new_unchecked(p as *mut u8);
            Ok((
                Self::from_base(class, n, cores, base, Backing::Mmap { len, _fd: fd }),
                raw,
            ))
        }
    }

    fn from_base(class: u8, n: u32, cores: usize, base: NonNull<u8>, backing: Backing) -> Self {
        let headers = base.cast::<SlotHeader>();
        // SAFETY: header_region is within the allocation and 4 KiB aligned (2 MiB multiple).
        let payloads =
            unsafe { NonNull::new_unchecked(base.as_ptr().add(Self::header_region(n as usize))) }
                .cast::<Payload<W>>();
        let cores = (0..cores).map(|_| CoreControl::default()).collect();
        SlotArena {
            class,
            n,
            headers,
            payloads,
            cores,
            backing,
            base,
        }
    }

    #[inline(always)]
    pub fn class(&self) -> u8 {
        self.class
    }
    #[inline(always)]
    pub fn len(&self) -> u32 {
        self.n
    }
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.n == 0
    }
    #[inline(always)]
    pub fn core(&self, core: u8) -> &CoreControl {
        &self.cores[core as usize]
    }
    #[inline(always)]
    pub fn header(&self, idx: u32) -> &SlotHeader {
        assert!(idx < self.n);
        // SAFETY: idx < n and the header array has n entries.
        unsafe { &*self.headers.as_ptr().add(idx as usize) }
    }
    #[inline(always)]
    pub fn payload(&self, idx: u32) -> &[AtomicU64; W] {
        assert!(idx < self.n);
        // SAFETY: idx < n and the payload array has n entries.
        unsafe { &(*self.payloads.as_ptr().add(idx as usize)).0 }
    }
    /// Stable DMA address of a payload (for `WRITE_FIXED` / `READ_FIXED`).
    #[inline(always)]
    pub fn payload_ptr(&self, idx: u32) -> *mut u8 {
        self.payload(idx).as_ptr() as *mut u8
    }
    /// Whole backing region (to register as io_uring fixed buffers).
    pub fn region(&self) -> (*mut u8, usize) {
        (self.base.as_ptr(), Self::region_bytes(self.n as usize))
    }

    #[inline(always)]
    fn resolve(&self, h: SlotHandle) -> Option<u32> {
        (!h.is_null() && h.class() == self.class && h.index() < self.n).then_some(h.index())
    }

    /// Generational, owner-validated optimistic read. This is the only read path; there is
    /// no API that reads a slot without checking `(owner, tenant)` inside the section.
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
        // `Err(())` inside the closure = validated owner mismatch.
        let r = read_deadline(
            &hdr.seq,
            words,
            &crate::MonotonicClock,
            std::time::Duration::from_micros(2),
            std::time::Duration::from_micros(1),
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
            crate::DeadlineReadError::WriterStalled { .. } => SlotReadError::WriterStalled,
            crate::DeadlineReadError::DeadlineExhausted { .. } => SlotReadError::DeadlineExhausted,
            crate::DeadlineReadError::Corrupt => SlotReadError::Corrupt,
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

    /// Owner core only: mutate in place under the seqlock and stamp the record LSN.
    /// `debug_assert` catches a stale handle on the writer side.
    #[inline]
    pub fn write<R>(
        &self,
        h: SlotHandle,
        lsn: u64,
        f: impl FnOnce(WordsMut<'_>) -> R,
    ) -> Result<R, crate::WriteError> {
        let idx = self.resolve(h).ok_or(crate::WriteError::Reassigned)?;
        let hdr = self.header(idx);
        let g = hdr
            .seq
            .write_begin()
            .map_err(|_| crate::WriteError::WriterBusy)?;
        if hdr.owner.load(Relaxed) != h.raw() {
            g.commit();
            return Err(crate::WriteError::Reassigned);
        }
        let current = hdr.applied_lsn.load(Relaxed);
        if lsn < current {
            g.commit();
            return Err(crate::WriteError::LsnRegression {
                current,
                attempted: lsn,
            });
        }
        let r = f(WordsMut::new(self.payload(idx)));
        hdr.applied_lsn.store(lsn, Relaxed);
        g.commit();
        Ok(r)
    }

    /// Owner core only, slot currently unowned: issue the next generation, install the
    /// owner, and expose it. The *caller* publishes the handle in the route table after
    /// this returns (publication order: slot first, route second).
    pub fn install(
        &self,
        idx: u32,
        tw: TenantWord,
        applied_lsn: u64,
        checkpoint_lsn: u64,
    ) -> Result<Option<SlotHandle>, WriterBusy> {
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        let last = hdr.last_generation.load(Relaxed);
        let Some(h) = SlotHandle::pack(idx, self.class, last + 1) else {
            g.commit();
            return Ok(None); // generation space exhausted: retire this slot
        };
        if hdr.owner.load(Relaxed) != 0 {
            g.commit();
            return Err(WriterBusy);
        }
        hdr.last_generation.store(h.generation(), Relaxed);
        hdr.tenant.store(tw.pack(), Relaxed);
        hdr.applied_lsn.store(applied_lsn, Relaxed);
        hdr.checkpoint_lsn.store(checkpoint_lsn, Relaxed);
        hdr.payload_len.store(Self::PAYLOAD_BYTES as u64, Relaxed);
        hdr.owner.store(h.raw(), Relaxed);
        g.commit();
        Ok(Some(h))
    }

    /// Owner core only: revoke ownership. Any reader still holding `h` gets `Reassigned`
    /// (either the owner check fails, or a section that straddled this one fails the
    /// sequence check). The caller must have unpublished the route entry first.
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

    /// Owner core only: write a header-only section (e.g. checkpoint LSN after a CQE).
    pub fn set_checkpoint_lsn(&self, h: SlotHandle, lsn: u64) -> Result<(), WriterBusy> {
        let hdr = self.header(self.resolve(h).expect("invalid handle"));
        let g = hdr.seq.write_begin()?;
        hdr.checkpoint_lsn.store(lsn, Relaxed);
        g.commit();
        Ok(())
    }

    /// Owner core only: writer view of a slot *that is not owned* (loading / zero-fill).
    pub fn with_unowned_payload<R>(
        &self,
        idx: u32,
        f: impl FnOnce(WordsMut<'_>) -> R,
    ) -> Result<R, WriterBusy> {
        let hdr = self.header(idx);
        let g = hdr.seq.write_begin()?;
        debug_assert_eq!(hdr.owner.load(Relaxed), 0);
        let r = f(WordsMut::new(self.payload(idx)));
        g.commit();
        Ok(r)
    }
}

impl<const W: usize> Drop for SlotArena<W> {
    fn drop(&mut self) {
        match &self.backing {
            // SAFETY: allocated in `new_heap` with exactly this layout.
            Backing::Heap { layout } => unsafe { std::alloc::dealloc(self.base.as_ptr(), *layout) },
            #[cfg(target_os = "linux")]
            // SAFETY: mapped in `new_hugetlb` with this length.
            Backing::Mmap { len, .. } => unsafe {
                libc::munmap(self.base.as_ptr().cast(), *len);
            },
        }
    }
}

// ---------------------------------------------------------------------------------
// Route table: 16 bytes per tenant, 4 entries per cache line, indexed by dense id
// ---------------------------------------------------------------------------------

/// Replaces the spec's 24-byte `#[repr(packed)]` entry, which cannot contain atomics and
/// forces unaligned loads. The tenant id is implicit (dense index), and the owning
/// slot's header re-validates it, so 16 bytes suffice: 1M tenants = 16 MiB.
#[derive(Debug, Default)]
#[repr(C, align(16))]
pub struct RouteEntry {
    /// `SlotHandle::raw()`; 0 = cold (not resident).
    pub handle: AtomicU64,
    /// `ExtentLoc` of the current on-disk image.
    pub extent: AtomicU64,
}
const _: () = assert!(core::mem::size_of::<RouteEntry>() == 16);

#[derive(Debug)]
pub struct RouteTable {
    entries: Box<[RouteEntry]>,
}

impl RouteTable {
    pub fn new(tenants: u32) -> Self {
        RouteTable {
            entries: (0..tenants).map(|_| RouteEntry::default()).collect(),
        }
    }
    /// Reader side (read-only mapping safe).
    #[inline(always)]
    pub fn lookup(&self, tenant: u32) -> Option<(SlotHandle, ExtentLoc)> {
        let e = self.entries.get(tenant as usize)?;
        let h = e.handle.load(Relaxed);
        let x = e.extent.load(Relaxed);
        fence(Acquire);
        Some((SlotHandle::from_raw(h), ExtentLoc(x)))
    }
    /// Owner core: publish after `SlotArena::install`.
    #[inline]
    pub fn publish(&self, tenant: u32, h: SlotHandle) {
        self.entries[tenant as usize].handle.store(h.raw(), Release);
    }
    /// Owner core: unpublish *before* `SlotArena::revoke`. CAS so a stale eviction can
    /// never unpublish a newer residency.
    #[inline]
    pub fn unpublish(&self, tenant: u32, h: SlotHandle) -> bool {
        self.entries[tenant as usize]
            .handle
            .compare_exchange(h.raw(), 0, Release, Relaxed)
            .is_ok()
    }
    #[inline]
    pub fn set_extent(&self, tenant: u32, loc: ExtentLoc) {
        self.entries[tenant as usize].extent.store(loc.0, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 512; // 4 KiB slots for tests

    #[test]
    fn handle_packing_round_trips_and_rejects_out_of_range() {
        let h = SlotHandle::pack(MAX_INDEX, MAX_CLASS, MAX_GENERATION).unwrap();
        assert_eq!(
            (h.index(), h.class(), h.generation()),
            (MAX_INDEX, MAX_CLASS, MAX_GENERATION)
        );
        assert!(
            h.successor().is_none(),
            "generation exhaustion retires the slot"
        );
        assert!(
            SlotHandle::pack(0, 0, 0).is_none(),
            "generation 0 is reserved for NULL"
        );
        assert!(SlotHandle::pack(MAX_INDEX + 1, 0, 1).is_none());
        assert!(SlotHandle::pack(0, MAX_CLASS + 1, 1).is_none());
        let a = SlotHandle::pack(7, 3, 41).unwrap();
        assert_eq!(a.successor().unwrap().generation(), 42);
        assert_eq!(a.successor().unwrap().index(), 7);
    }

    #[test]
    fn stale_handle_cannot_read_new_tenant() {
        let arena = SlotArena::<W>::new_heap(0, 4, 2);
        let route = RouteTable::new(16);
        let tw = |t| TenantWord {
            tenant: t,
            core: 1,
            layout_version: 1,
            flags: 0,
        };

        let h1 = arena.install(2, tw(5), 10, 10).unwrap().unwrap();
        route.publish(5, h1);
        arena.write(h1, 11, |w| w.set(8, 0xAAAA)).unwrap();
        assert_eq!(
            arena
                .read(h1, 5, Visibility::Applied, |v| v.word(8))
                .unwrap()
                .value,
            0xAAAA
        );

        // Evict tenant 5, reuse slot 2 for tenant 9.
        assert!(route.unpublish(5, h1));
        arena.revoke(h1).unwrap();
        let h2 = arena.install(2, tw(9), 0, 0).unwrap().unwrap();
        route.publish(9, h2);
        arena.write(h2, 1, |w| w.set(8, 0xBBBB)).unwrap();

        assert_eq!(h1.index(), h2.index());
        assert_eq!(h2.generation(), h1.generation() + 1);
        assert_eq!(
            arena.read(h1, 5, Visibility::Applied, |v| v.word(8)),
            Err(SlotReadError::Reassigned)
        );
        // Even a forged (old handle, new tenant) pair fails on the generation.
        assert_eq!(
            arena.read(h1, 9, Visibility::Applied, |v| v.word(8)),
            Err(SlotReadError::Reassigned)
        );
        assert_eq!(
            arena
                .read(h2, 9, Visibility::Applied, |v| v.word(8))
                .unwrap()
                .value,
            0xBBBB
        );
        assert!(!route.unpublish(5, h1), "stale unpublish is a no-op");
    }

    #[test]
    fn durable_visibility_follows_core_watermark() {
        let arena = SlotArena::<W>::new_heap(0, 1, 1);
        let tw = TenantWord {
            tenant: 1,
            core: 0,
            layout_version: 1,
            flags: 0,
        };
        let h = arena.install(0, tw, 0, 0).unwrap().unwrap();
        arena.write(h, 7, |w| w.set(9, 1)).unwrap();
        assert_eq!(
            arena.read(h, 1, Visibility::Durable, |v| v.word(9)),
            Err(SlotReadError::NotYetDurable {
                applied: 7,
                durable: 0
            })
        );
        assert!(arena.read(h, 1, Visibility::Applied, |v| v.word(9)).is_ok());
        arena.core(0).publish_durable(7);
        assert_eq!(
            arena
                .read(h, 1, Visibility::Durable, |v| v.word(9))
                .unwrap()
                .value,
            1
        );
    }

    #[test]
    fn extent_loc_ab_flip() {
        let l = ExtentLoc::new(3, 1000, false);
        assert_eq!(l.current_offset(65536), 1000 * 4096);
        assert_eq!(l.flipped().current_offset(65536), 1000 * 4096 + 65536);
        assert_eq!(l.flipped().flipped(), l);
        assert_eq!(l.container(), 3);
    }
}
