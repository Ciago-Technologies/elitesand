//! CQE-gated slot lifecycle (eviction / checkpoint / load state machine), one per core.
//!
//! Spec defect: "submit batched `IORING_OP_WRITE_FIXED` requests ... immediately marking
//! those slots free for reuse". The device DMAs from the slot *after* submission, at any
//! time until the CQE. Reusing the slot before the CQE writes the next tenant's bytes
//! into the previous tenant's extent (silent cross-tenant corruption), and a failed
//! write loses the only copy of dirty state.
//!
//! State machine (core-local, single-threaded, no atomics except shm header/route):
//!
//! ```text
//!                    admit_new()                      admit_load()
//!   +------+ ------------------------------+   +------+ -----------> +---------+
//!   | Free |                               |   | Free |              | Loading |
//!   +------+ <------------------+          |   +------+ <-- CQE err  +---------+
//!      ^                        |          v                  / bad image  |
//!      | io_inflight == 0       |    +----------+   CQE ok + image valid   |
//!      | && pins == 0           |    | Resident | <------------------------+
//!      |                        |    | (dirty?) | --- mutate -> dirty, last_lsn
//!  +-----------+  unpublish +   |    +----------+
//!  | Quiescing | <- revoke  ----+--- clean victim
//!  +-----------+                |         | dirty victim, last_lsn <= durable_lsn (WAL rule)
//!      ^                        |         v
//!      |                        |    +----------+   mutations -> Defer (write frozen)
//!      +--- CQE ok, evict, -----+--- | Flushing |   CQE err/short -> Resident(dirty), retry
//!           no deferred              +----------+   CQE ok + deferred -> Resident(clean), Resume
//! ```
//!
//! Invariants (asserted):
//! * I1 A slot enters the free list only when `io_inflight == 0 && pins == 0` and its
//!   handle has been unpublished *and* revoked (generation retired).
//! * I2 Slot memory is never written by the CPU while a write SQE that reads it is in
//!   flight (Flushing freezes mutations; Loading slots are unowned).
//! * I3 A dirty image is written only after every log record it contains is durable.
//! * I4 Checkpoints go to the *other* A/B copy; the route's extent location flips only
//!   after a full-length successful CQE. A torn checkpoint never destroys the last image.
//! * I5 CQEs are matched by `(object, ticket)`; a stale CQE never changes state.

use crate::envelope::{ExtentImage, ImageStatus};
use crate::io::{IoBackend, Op, SqFull, UserData};
use crate::slot::{ExtentLoc, IMAGE_HEADER_WORDS, RouteTable, SlotArena, SlotHandle, TenantWord};
use core::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Residency {
    pub tenant: u32,
    pub handle: SlotHandle,
    pub dirty: bool,
    /// LSN of the last record applied to this slot.
    pub last_lsn: u64,
    /// LSN folded into the current on-disk image.
    pub ckpt_lsn: u64,
    pub image_seq: u64,
    pub loc: ExtentLoc,
    pub layout_version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// Normal path: read the copy the route table says is current.
    Current,
    /// Crash recovery (route table lost): read A, then B, keep the newer valid one.
    RecoverA,
    RecoverB {
        a: ImageStatus,
    },
    RecoverRereadA {
        a: ExtentImage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Free,
    Loading {
        tenant: u32,
        loc: ExtentLoc,
        probe: Probe,
        layout_version: u8,
    },
    Resident(Residency),
    Replaying {
        res: Residency,
        through_lsn: u64,
    },
    Flushing {
        res: Residency,
        target: ExtentLoc,
        lsn: u64,
        image_seq: u64,
        evict: bool,
        deferred: u32,
    },
    Quiescing {
        tenant: u32,
    },
    Retired,
}

/// Trusted recovered log interval; CRC images alone cannot prove log coverage.
#[derive(Debug, Clone, Copy)]
pub struct RecoveryBounds {
    pub current_epoch: u64,
    pub expected_image_epoch: Option<u64>,
    pub wal_start_lsn: u64,
    pub wal_end_lsn: u64,
}
impl RecoveryBounds {
    fn covers(&self, checkpoint: u64) -> bool {
        self.wal_start_lsn > 0
            && self.wal_start_lsn - 1 <= checkpoint
            && checkpoint <= self.wal_end_lsn
    }
    /// Normal (non-crash) cold load of a tenant whose durable metadata records that its
    /// image at `checkpoint_lsn` contains every record it has logged. The required log
    /// interval is empty, so WAL truncation below it is irrelevant; an older or newer
    /// image on disk fails coverage and requests repair (R7-2).
    pub fn clean(current_epoch: u64, image_epoch: u64, checkpoint_lsn: u64) -> Self {
        Self {
            current_epoch,
            expected_image_epoch: Some(image_epoch),
            wal_start_lsn: checkpoint_lsn.checked_add(1).unwrap_or(0),
            wal_end_lsn: checkpoint_lsn,
        }
    }
}
#[derive(Debug, Clone, Copy)]
struct Meta {
    incarnation: u128,
    recovery: RecoveryBounds,
    state: State,
    ticket: u32,
    io_inflight: u8,
    pins: u16,
    retries: u8,
    referenced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    NeedsRepair {
        tenant: u32,
    },
    Absent {
        tenant: u32,
    },
    ReplayRequired {
        tenant: u32,
        handle: SlotHandle,
        from_lsn: u64,
        through_lsn: u64,
    },
    /// Route published only after the required recovered log suffix has been applied.
    Loaded {
        tenant: u32,
        handle: SlotHandle,
        checkpoint_lsn: u64,
    },
    /// Negative I/O/validation error. Only all-zero image pairs emit Absent.
    LoadFailed {
        tenant: u32,
        errno: i32,
    },
    /// Image with all records `<= lsn` is durable at `loc`.
    Checkpointed {
        tenant: u32,
        lsn: u64,
        loc: ExtentLoc,
    },
    /// Residency ended (route unpublished, generation retired).
    Evicted {
        tenant: u32,
        slot: u32,
    },
    /// Slot returned to the free list.
    Freed {
        slot: u32,
    },
    /// Mutations deferred during a flush may now run.
    Resume {
        tenant: u32,
        handle: SlotHandle,
        deferred: u32,
    },
    FlushFailed {
        tenant: u32,
        res: i32,
        retries: u8,
    },
    /// Flush retries exhausted: slot is pinned in memory, capacity shrinks, alert.
    DiskDegraded {
        tenant: u32,
    },
    SlotRetired {
        slot: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
    /// No free slot. Caller replies ERR_BACKPRESSURE; nothing was allocated.
    NoFreeSlot {
        flushing: u32,
    },
    SqFull,
    InvalidRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    InvalidMutation,
    Proceed,
    /// Slot is being flushed; the executor parks the message and re-runs it on `Resume`.
    Defer,
    /// Handle no longer resident: re-resolve the tenant.
    Stale,
}

#[derive(Debug, Clone, Copy)]
pub struct LifecycleConfig {
    /// Start background eviction below this many free slots.
    pub low_free: u32,
    /// Stop evicting once this many slots are free (or in flight to become free).
    pub high_free: u32,
    pub max_flush_inflight: u32,
    pub max_retries: u8,
    /// Current log epoch, stamped into images.
    pub epoch: u64,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub stale_cqes: u64,
    pub flushes: u64,
    pub flush_errors: u64,
    pub evictions: u64,
    pub deferred: u64,
    pub wal_blocked: u64,
}

pub struct SlotLifecycle<'a, const W: usize> {
    arena: &'a SlotArena<W>,
    route: &'a RouteTable,
    core: u8,
    range: Range<u32>,
    meta: Box<[Meta]>,
    free: Vec<u32>,
    hand: usize,
    flushing: u32,
    cfg: LifecycleConfig,
    pub stats: Stats,
}

impl<const W: usize> core::fmt::Debug for SlotLifecycle<'_, W> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotLifecycle")
            .field("core", &self.core)
            .field("free", &self.free.len())
            .field("flushing", &self.flushing)
            .finish()
    }
}

const EXTENT_BYTES_MAX: usize = u32::MAX as usize;

impl<const W: usize> Drop for SlotLifecycle<'_, W> {
    /// R9-1: dropping with device I/O outstanding fences the core so a late DMA cannot be
    /// served as valid data; the arena keeps its memory alive while `io_pending > 0`.
    fn drop(&mut self) {
        if self.meta.iter().any(|m| m.io_inflight > 0) {
            self.arena.core(self.core).fence();
        }
    }
}

impl<'a, const W: usize> SlotLifecycle<'a, W> {
    pub const EXTENT_BYTES: usize = W * 8;
    /// True when no load or checkpoint I/O from this lifecycle is outstanding.
    pub fn quiescent(&self) -> bool {
        self.meta.iter().all(|m| m.io_inflight == 0)
    }
    /// Declare that the device holds no pointer into this lifecycle's payloads (ring
    /// destroyed or every operation reaped), so teardown neither fences nor leaks.
    /// Outstanding operations are abandoned; call only before dropping the lifecycle.
    ///
    /// # Safety
    /// No submitted load or checkpoint may still read or write its payload.
    pub unsafe fn release_device_references(&mut self) {
        for i in 0..self.meta.len() {
            while self.meta[i].io_inflight > 0 {
                self.meta[i].io_inflight -= 1;
                self.arena.io_completed();
            }
        }
    }

    pub fn new(
        arena: &'a SlotArena<W>,
        route: &'a RouteTable,
        core: u8,
        range: Range<u32>,
        cfg: LifecycleConfig,
    ) -> Self {
        assert!(
            range.end <= arena.len() && !range.is_empty() && Self::EXTENT_BYTES <= EXTENT_BYTES_MAX
        );
        assert!(cfg.low_free <= cfg.high_free && cfg.max_flush_inflight >= 1);
        let n = range.len();
        let meta = vec![
            Meta {
                state: State::Free,
                ticket: 0,
                io_inflight: 0,
                pins: 0,
                retries: 0,
                referenced: false,
                incarnation: 0,
                recovery: RecoveryBounds {
                    current_epoch: cfg.epoch,
                    expected_image_epoch: None,
                    wal_start_lsn: 1,
                    wal_end_lsn: 0
                }
            };
            n
        ]
        .into_boxed_slice();
        let free = range.clone().rev().collect();
        SlotLifecycle {
            arena,
            route,
            core,
            range,
            meta,
            free,
            hand: 0,
            flushing: 0,
            cfg,
            stats: Stats::default(),
        }
    }

    #[inline]
    fn m(&mut self, idx: u32) -> &mut Meta {
        &mut self.meta[(idx - self.range.start) as usize]
    }

    pub fn state(&self, idx: u32) -> State {
        self.meta[(idx - self.range.start) as usize].state
    }
    pub fn free_slots(&self) -> u32 {
        self.free.len() as u32
    }
    pub fn flushing(&self) -> u32 {
        self.flushing
    }
    pub fn set_epoch(&mut self, epoch: u64) {
        self.cfg.epoch = epoch;
    }

    fn handle_idx(&self, h: SlotHandle) -> Option<u32> {
        let idx = h.index();
        (h.class() == self.arena.class() && self.range.contains(&idx)).then_some(idx)
    }

    fn residency(&self, h: SlotHandle) -> Option<(u32, Residency)> {
        let idx = self.handle_idx(h)?;
        match self.state(idx) {
            State::Resident(r) | State::Flushing { res: r, .. } if r.handle == h => Some((idx, r)),
            _ => None,
        }
    }

    fn tw(&self, tenant: u32, layout_version: u8) -> TenantWord {
        TenantWord {
            tenant,
            core: self.core,
            layout_version,
            flags: 0,
        }
    }

    fn push_free(&mut self, idx: u32, events: &mut Vec<Event>) {
        assert_eq!(
            self.arena
                .header(idx)
                .owner
                .load(core::sync::atomic::Ordering::Relaxed),
            0,
            "I1 violated: slot still owned"
        );
        let m = self.m(idx);
        assert!(
            m.io_inflight == 0 && m.pins == 0,
            "I1 violated: freeing slot {idx} with I/O or pins outstanding"
        );
        assert!(
            matches!(m.state, State::Quiescing { .. } | State::Loading { .. }),
            "I1 violated: free from {:?}",
            m.state
        );
        m.state = State::Free;
        m.retries = 0;
        m.referenced = false;
        self.free.push(idx);
        events.push(Event::Freed { slot: idx });
    }

    fn try_finish_quiesce(&mut self, idx: u32, events: &mut Vec<Event>) {
        let m = *self.m(idx);
        if matches!(m.state, State::Quiescing { .. }) && m.io_inflight == 0 && m.pins == 0 {
            self.push_free(idx, events);
        }
    }

    /// End residency: route first (new lookups go cold), then revoke the generation
    /// (in-flight readers fail validation), then wait for I/O and pins.
    fn end_residency(&mut self, idx: u32, r: Residency, events: &mut Vec<Event>) {
        let unpublished = self.route.unpublish(r.tenant, r.handle);
        debug_assert!(unpublished, "route pointed elsewhere");
        self.arena
            .revoke(r.handle)
            .expect("owner core is the only writer");
        self.m(idx).state = State::Quiescing { tenant: r.tenant };
        self.stats.evictions += 1;
        events.push(Event::Evicted {
            tenant: r.tenant,
            slot: idx,
        });
        self.try_finish_quiesce(idx, events);
    }

    fn install(
        &mut self,
        idx: u32,
        tenant: u32,
        layout_version: u8,
        applied: u64,
        ckpt: u64,
        events: &mut Vec<Event>,
    ) -> Option<SlotHandle> {
        match self
            .arena
            .install(
                idx,
                self.tw(tenant, layout_version),
                self.meta[(idx - self.range.start) as usize].incarnation,
                applied,
                ckpt,
            )
            .expect("owner core is the only writer")
        {
            Some(h) => Some(h),
            None => {
                self.m(idx).state = State::Retired;
                events.push(Event::SlotRetired { slot: idx });
                None
            }
        }
    }

    // ----------------------------------------------------------------- admission

    /// Brand-new tenant: zeroed payload, resident and dirty immediately. `loc` is a fresh
    /// extent pair from the extent allocator; `lsn` is the creating record's LSN.
    pub fn admit_new(
        &mut self,
        tenant: u32,
        incarnation: u128,
        loc: ExtentLoc,
        lsn: u64,
        layout_version: u8,
        events: &mut Vec<Event>,
    ) -> Result<SlotHandle, AdmitError> {
        loop {
            let idx = self.free.pop().ok_or(AdmitError::NoFreeSlot {
                flushing: self.flushing,
            })?;
            self.m(idx).incarnation = incarnation;
            self.arena
                .with_unowned_payload(idx, |w| w.fill(0))
                .expect("owner core is the only writer");
            let Some(h) = self.install(idx, tenant, layout_version, lsn, 0, events) else {
                continue;
            };
            self.route.set_extent(tenant, loc);
            self.route.publish(tenant, h);
            let m = self.m(idx);
            m.state = State::Resident(Residency {
                tenant,
                handle: h,
                dirty: true,
                last_lsn: lsn,
                ckpt_lsn: 0,
                image_seq: 0,
                loc,
                layout_version,
            });
            m.referenced = true;
            return Ok(h);
        }
    }

    /// Cold tenant: read its extent into a free slot. The slot stays unowned (readers
    /// with any older handle fail the owner check) until the image validates.
    pub fn admit_load<I: IoBackend>(
        &mut self,
        tenant: u32,
        incarnation: u128,
        recovery: RecoveryBounds,
        loc: ExtentLoc,
        layout_version: u8,
        recover: bool,
        io: &mut I,
    ) -> Result<u32, AdmitError> {
        if recovery.current_epoch == 0
            || recovery.wal_start_lsn == 0
            || recovery.wal_end_lsn < recovery.wal_start_lsn - 1
        {
            return Err(AdmitError::InvalidRecovery);
        }
        let idx = self.free.pop().ok_or(AdmitError::NoFreeSlot {
            flushing: self.flushing,
        })?;
        self.m(idx).incarnation = incarnation;
        self.m(idx).recovery = recovery;
        let probe = if recover {
            Probe::RecoverA
        } else {
            Probe::Current
        };
        self.m(idx).state = State::Loading {
            tenant,
            loc,
            probe,
            layout_version,
        };
        if self.submit_load(idx, loc, probe, io).is_err() {
            self.m(idx).state = State::Free;
            self.free.push(idx);
            return Err(AdmitError::SqFull);
        }
        Ok(idx)
    }

    fn probe_offset(loc: ExtentLoc, probe: Probe) -> u64 {
        let b = Self::EXTENT_BYTES as u64;
        match probe {
            Probe::Current => loc.current_offset(b),
            Probe::RecoverA | Probe::RecoverRereadA { .. } => loc.base_block() * 4096,
            Probe::RecoverB { .. } => loc.base_block() * 4096 + b,
        }
    }

    fn submit_load<I: IoBackend>(
        &mut self,
        idx: u32,
        loc: ExtentLoc,
        probe: Probe,
        io: &mut I,
    ) -> Result<(), SqFull> {
        let m = self.m(idx);
        m.ticket = m.ticket.wrapping_add(1);
        let ud = UserData {
            op: Op::Load,
            object: idx,
            ticket: m.ticket,
        }
        .encode();
        // SAFETY: the slot is unowned and not on the free list until this CQE (I1/I2).
        self.arena.io_submitted();
        self.m(idx).io_inflight += 1;
        let result = unsafe {
            io.read_at(
                ud,
                loc.container(),
                self.arena.payload_ptr(idx),
                Self::EXTENT_BYTES as u32,
                Self::probe_offset(loc, probe),
            )
        };
        if let Err(e) = result {
            self.m(idx).io_inflight -= 1;
            self.arena.io_completed();
            return Err(e);
        }
        Ok(())
    }

    fn image_of(&self, idx: u32, tenant: u32) -> ImageStatus {
        // SAFETY: the read CQE completed and this unowned payload has no readers/writers.
        let bytes =
            unsafe { core::slice::from_raw_parts(self.arena.payload_ptr(idx), Self::EXTENT_BYTES) };
        let m = &self.meta[(idx - self.range.start) as usize];
        ExtentImage::inspect(
            bytes,
            tenant,
            m.incarnation,
            m.recovery.current_epoch,
            m.recovery.expected_image_epoch,
        )
    }

    fn on_load_cqe<I: IoBackend>(
        &mut self,
        idx: u32,
        res: i32,
        io: &mut I,
        events: &mut Vec<Event>,
    ) {
        let State::Loading {
            tenant,
            loc,
            probe,
            layout_version,
        } = self.state(idx)
        else {
            self.stats.stale_cqes += 1;
            return;
        };
        let fail = |s: &mut Self, errno: i32, events: &mut Vec<Event>| {
            events.push(Event::LoadFailed { tenant, errno });
            if s.m(idx).io_inflight == 0 {
                s.push_free(idx, events);
            }
        };
        if res < 0 {
            return fail(self, res, events);
        }
        if res as usize != Self::EXTENT_BYTES {
            // Short read is transient, not evidence of a bad image: re-read the *same*
            // copy. Falling back to the other (older) copy here would silently load
            // stale state.
            let m = self.m(idx);
            m.retries = m.retries.saturating_add(1);
            if m.retries > self.cfg.max_retries || self.submit_load(idx, loc, probe, io).is_err() {
                return fail(self, -libc::EIO, events);
            }
            return;
        }
        let img = self.image_of(idx, tenant);
        let repair = |s: &mut Self, events: &mut Vec<Event>| {
            events.push(Event::NeedsRepair { tenant });
            fail(s, -libc::EUCLEAN, events);
        };
        let next = match (probe, img) {
            (Probe::Current, ImageStatus::Valid(i)) => Ok((i, loc)),
            (Probe::Current, _) => {
                repair(self, events);
                return;
            }
            (Probe::RecoverA, a) => Err(Probe::RecoverB { a }),
            (Probe::RecoverB { a }, b) => match (a, b) {
                (ImageStatus::Absent, ImageStatus::Absent) => {
                    if self.m(idx).recovery.expected_image_epoch.is_some() {
                        repair(self, events);
                        return;
                    }
                    events.push(Event::Absent { tenant });
                    self.push_free(idx, events);
                    return;
                }
                (ImageStatus::Valid(a), ImageStatus::Valid(b)) if a.image_seq > b.image_seq => {
                    Err(Probe::RecoverRereadA { a })
                }
                (_, ImageStatus::Valid(b)) => {
                    Ok((b, ExtentLoc::new(loc.container(), loc.base_block(), true)))
                }
                (ImageStatus::Valid(a), _) => Err(Probe::RecoverRereadA { a }),
                _ => {
                    repair(self, events);
                    return;
                }
            },
            (Probe::RecoverRereadA { a }, ImageStatus::Valid(i)) if i == a => {
                Ok((i, ExtentLoc::new(loc.container(), loc.base_block(), false)))
            }
            (Probe::RecoverRereadA { .. }, _) => {
                repair(self, events);
                return;
            }
        };
        match next {
            Err(probe) => {
                self.m(idx).state = State::Loading {
                    tenant,
                    loc,
                    probe,
                    layout_version,
                };
                if self.submit_load(idx, loc, probe, io).is_err() {
                    fail(self, -libc::EAGAIN, events);
                }
            }
            Ok((img, used)) => {
                if !self.m(idx).recovery.covers(img.checkpoint_lsn) {
                    repair(self, events);
                    return;
                }
                let Some(h) = self.install(
                    idx,
                    tenant,
                    layout_version,
                    img.checkpoint_lsn,
                    img.checkpoint_lsn,
                    events,
                ) else {
                    events.push(Event::LoadFailed {
                        tenant,
                        errno: -libc::EAGAIN,
                    });
                    return;
                };
                let res = Residency {
                    tenant,
                    handle: h,
                    dirty: false,
                    last_lsn: img.checkpoint_lsn,
                    ckpt_lsn: img.checkpoint_lsn,
                    image_seq: img.image_seq,
                    loc: used,
                    layout_version,
                };
                let through_lsn = self.m(idx).recovery.wal_end_lsn;
                self.m(idx).state = State::Replaying { res, through_lsn };
                if through_lsn > img.checkpoint_lsn {
                    events.push(Event::ReplayRequired {
                        tenant,
                        handle: h,
                        from_lsn: img.checkpoint_lsn + 1,
                        through_lsn,
                    });
                } else {
                    self.finish_replay(h, events);
                }
            }
        }
    }

    /// Trusted replay driver applies only validated committed records for this identity.
    pub fn replay<R>(
        &mut self,
        h: SlotHandle,
        lsn: u64,
        f: impl FnOnce(crate::WordsMut<'_>) -> R,
    ) -> Result<R, Gate> {
        let idx = self.handle_idx(h).ok_or(Gate::Stale)?;
        match self.state(idx) {
            State::Replaying { res, through_lsn } if res.handle == h && lsn <= through_lsn => self
                .arena
                .write(h, lsn, f)
                .map_err(|_| Gate::InvalidMutation),
            _ => Err(Gate::Stale),
        }
    }
    /// A failed replay or replayed deletion must never publish a partial actor.
    pub fn abort_replay(&mut self, h: SlotHandle, events: &mut Vec<Event>) -> bool {
        let Some(idx) = self.handle_idx(h) else {
            return false;
        };
        let State::Replaying { res, .. } = self.state(idx) else {
            return false;
        };
        if res.handle != h || self.arena.revoke(h).is_err() {
            return false;
        }
        self.m(idx).state = State::Quiescing { tenant: res.tenant };
        self.try_finish_quiesce(idx, events);
        true
    }
    /// Called after scanning the complete trusted suffix, including no-op LSN gaps.
    pub fn finish_replay(&mut self, h: SlotHandle, events: &mut Vec<Event>) -> bool {
        let Some(idx) = self.handle_idx(h) else {
            return false;
        };
        let State::Replaying {
            mut res,
            through_lsn,
        } = self.state(idx)
        else {
            return false;
        };
        if res.handle != h
            || self.arena.core(self.core).is_fenced()
            || self
                .arena
                .header(idx)
                .applied_lsn
                .load(core::sync::atomic::Ordering::Acquire)
                != through_lsn
        {
            return false;
        }
        res.last_lsn = through_lsn;
        res.dirty = through_lsn > res.ckpt_lsn;
        let m = self.m(idx);
        m.state = State::Resident(res);
        m.referenced = true;
        m.retries = 0;
        self.route.set_extent(res.tenant, res.loc);
        self.route.publish(res.tenant, h);
        events.push(Event::Loaded {
            tenant: res.tenant,
            handle: h,
            checkpoint_lsn: res.ckpt_lsn,
        });
        true
    }

    // ----------------------------------------------------------------- mutation path

    pub fn gate(&mut self, h: SlotHandle) -> Gate {
        match self.residency(h) {
            None => Gate::Stale,
            Some((idx, _)) => match &mut self.m(idx).state {
                State::Flushing { deferred, .. } => {
                    *deferred += 1;
                    self.stats.deferred += 1;
                    Gate::Defer
                }
                _ => Gate::Proceed,
            },
        }
    }

    /// Apply a mutation whose log record has LSN `lsn` (already appended to the
    /// group-commit log). Returns `Err(gate)` if it cannot run now.
    pub fn mutate<R>(
        &mut self,
        h: SlotHandle,
        lsn: u64,
        f: impl FnOnce(crate::seqlock::WordsMut<'_>) -> R,
    ) -> Result<R, Gate> {
        match self.gate(h) {
            Gate::Proceed => {}
            g => return Err(g),
        }
        let idx = h.index();
        let r = self
            .arena
            .write(h, lsn, f)
            .map_err(|_| Gate::InvalidMutation)?;
        let m = self.m(idx);
        if let State::Resident(res) = &mut m.state {
            res.dirty = true;
            res.last_lsn = lsn;
        }
        m.referenced = true;
        Ok(r)
    }

    pub fn touch(&mut self, h: SlotHandle) {
        if let Some((idx, _)) = self.residency(h) {
            self.m(idx).referenced = true;
        }
    }

    /// Pin for zero-copy egress (e.g. `IORING_OP_SEND_ZC` from slot memory). Release with
    /// `unpin` on the *notification* CQE (`IORING_CQE_F_NOTIF`), not the first CQE.
    pub fn pin(&mut self, h: SlotHandle) -> bool {
        match self.residency(h) {
            Some((idx, _)) => {
                self.m(idx).pins += 1;
                true
            }
            None => false,
        }
    }

    pub fn unpin(&mut self, idx: u32, events: &mut Vec<Event>) {
        if !self.range.contains(&idx) {
            self.stats.stale_cqes += 1;
            return;
        }
        let m = self.m(idx);
        if m.pins == 0 {
            self.stats.stale_cqes += 1;
            return;
        }
        m.pins -= 1;
        self.try_finish_quiesce(idx, events);
    }

    // ----------------------------------------------------------------- flush / evict

    /// Checkpoint without evicting (lets the log truncate). Returns false if not
    /// eligible right now (not resident, clean, WAL-blocked, SQ full).
    pub fn checkpoint<I: IoBackend>(
        &mut self,
        h: SlotHandle,
        durable_lsn: u64,
        io: &mut I,
    ) -> bool {
        match self.residency(h) {
            Some((idx, r))
                if matches!(self.state(idx), State::Resident(_))
                    && r.dirty
                    && r.last_lsn <= durable_lsn =>
            {
                self.start_flush(idx, r, false, io).is_ok()
            }
            _ => false,
        }
    }

    fn start_flush<I: IoBackend>(
        &mut self,
        idx: u32,
        r: Residency,
        evict: bool,
        io: &mut I,
    ) -> Result<(), SqFull> {
        debug_assert!(r.dirty);
        let image_seq = r.image_seq + 1;
        let lsn = r.last_lsn;
        let img = ExtentImage {
            tenant: r.tenant,
            incarnation: self.meta[(idx - self.range.start) as usize].incarnation,
            tombstone: false,
            layout_version: r.layout_version as u32,
            checkpoint_lsn: lsn,
            epoch: self.cfg.epoch,
            image_seq,
        };
        // Stamp the image header inside a write section (readers see it atomically).
        // SAFETY: we are the only writer; reading our own payload non-atomically is race-free.
        let body = unsafe {
            core::slice::from_raw_parts(
                self.arena.payload_ptr(idx).add(IMAGE_HEADER_WORDS * 8),
                Self::EXTENT_BYTES - IMAGE_HEADER_WORDS * 8,
            )
        };
        let hw = img.header_words(body);
        self.arena
            .stamp_image_header(r.handle, &hw)
            .expect("owner core is the only writer");
        let target = r.loc.flipped();
        let m = self.m(idx);
        let ticket = m.ticket.wrapping_add(1);
        let ud = UserData {
            op: Op::Checkpoint,
            object: idx,
            ticket,
        }
        .encode();
        // SAFETY: Flushing freezes CPU writes to this payload until the CQE (I2), and the
        // slot cannot be freed while io_inflight > 0 (I1).
        self.arena.io_submitted();
        self.m(idx).io_inflight += 1;
        let result = unsafe {
            io.write_at(
                ud,
                target.container(),
                self.arena.payload_ptr(idx),
                Self::EXTENT_BYTES as u32,
                target.current_offset(Self::EXTENT_BYTES as u64),
                true,
            )
        };
        if let Err(e) = result {
            self.m(idx).io_inflight -= 1;
            self.arena.io_completed();
            return Err(e);
        }
        let m = self.m(idx);
        m.ticket = ticket;
        m.state = State::Flushing {
            res: r,
            target,
            lsn,
            image_seq,
            evict,
            deferred: 0,
        };
        self.flushing += 1;
        self.stats.flushes += 1;
        Ok(())
    }

    /// Background eviction pass (CLOCK second-chance). Never frees anything itself for
    /// dirty slots: it only *starts* flushes; slots become free in `on_cqe`.
    /// Returns the number of slots that became free or started flushing.
    pub fn run_eviction<I: IoBackend>(
        &mut self,
        durable_lsn: u64,
        io: &mut I,
        events: &mut Vec<Event>,
    ) -> u32 {
        if self.free_slots() >= self.cfg.low_free {
            return 0;
        }
        let n = self.meta.len();
        let mut progressed = 0;
        for _ in 0..2 * n {
            if self.free_slots() + self.flushing >= self.cfg.high_free
                || self.flushing >= self.cfg.max_flush_inflight
            {
                break;
            }
            let i = self.hand;
            self.hand = (self.hand + 1) % n;
            let idx = self.range.start + i as u32;
            let m = self.meta[i];
            let State::Resident(r) = m.state else {
                continue;
            };
            if m.pins > 0 || m.retries >= self.cfg.max_retries {
                continue;
            }
            if m.referenced {
                self.meta[i].referenced = false;
                continue;
            }
            if !r.dirty {
                self.end_residency(idx, r, events);
                progressed += 1;
            } else if r.last_lsn > durable_lsn {
                self.stats.wal_blocked += 1; // I3: wait for the group commit
            } else if self.start_flush(idx, r, true, io).is_ok() {
                progressed += 1;
            } else {
                break; // SQ full: try next tick
            }
        }
        progressed
    }

    // ----------------------------------------------------------------- completions

    /// Feed every CQE whose op is `Load` or `Checkpoint` here.
    ///
    /// # Safety
    /// A matching token/result must be a genuine completed request owned by this
    /// lifecycle; the backend must no longer access its buffer. Proven stale tokens
    /// are allowed only when they cannot match a currently pending request.
    /// ```compile_fail
    /// use coagomem_core::{lifecycle::SlotLifecycle,io::IoBackend};
    /// fn forged(owner:&mut SlotLifecycle<'_,512>,io:&mut impl IoBackend) {
    ///     owner.on_cqe(0,4096,io,&mut Vec::new());
    /// }
    /// ```
    pub unsafe fn on_cqe<I: IoBackend>(
        &mut self,
        user_data: u64,
        res: i32,
        io: &mut I,
        events: &mut Vec<Event>,
    ) {
        let Some(ud) = UserData::decode(user_data) else {
            return;
        };
        if !self.range.contains(&ud.object) {
            self.stats.stale_cqes += 1;
            return;
        }
        let idx = ud.object;
        let m = self.m(idx);
        let current = m.ticket == ud.ticket;
        let expected = matches!(
            (ud.op, m.state),
            (Op::Load, State::Loading { .. }) | (Op::Checkpoint, State::Flushing { .. })
        );
        if !current || !expected || m.io_inflight == 0 {
            self.stats.stale_cqes += 1;
            return;
        }
        m.io_inflight -= 1;
        let state = m.state;
        self.arena.io_completed();
        match (ud.op, state) {
            (Op::Load, State::Loading { .. }) if current => self.on_load_cqe(idx, res, io, events),
            (
                Op::Checkpoint,
                State::Flushing {
                    res: r,
                    target,
                    lsn,
                    image_seq,
                    evict,
                    deferred,
                },
            ) if current => {
                self.flushing -= 1;
                if res >= 0 && res as usize == Self::EXTENT_BYTES {
                    let nr = Residency {
                        dirty: false,
                        ckpt_lsn: lsn,
                        image_seq,
                        loc: target,
                        ..r
                    };
                    self.route.set_extent(r.tenant, target);
                    self.arena
                        .set_checkpoint_lsn(r.handle, lsn)
                        .expect("owner core is the only writer");
                    self.m(idx).retries = 0;
                    events.push(Event::Checkpointed {
                        tenant: r.tenant,
                        lsn,
                        loc: target,
                    });
                    if deferred > 0 {
                        self.m(idx).state = State::Resident(nr);
                        events.push(Event::Resume {
                            tenant: r.tenant,
                            handle: r.handle,
                            deferred,
                        });
                    } else if evict {
                        self.end_residency(idx, nr, events);
                    } else {
                        self.m(idx).state = State::Resident(nr);
                    }
                } else {
                    // Error or short write: the in-memory copy is still the only good one.
                    self.stats.flush_errors += 1;
                    let m = self.m(idx);
                    m.retries = m.retries.saturating_add(1);
                    let retries = m.retries;
                    m.state = State::Resident(r);
                    events.push(Event::FlushFailed {
                        tenant: r.tenant,
                        res,
                        retries,
                    });
                    if deferred > 0 {
                        events.push(Event::Resume {
                            tenant: r.tenant,
                            handle: r.handle,
                            deferred,
                        });
                    }
                    if retries == self.cfg.max_retries {
                        events.push(Event::DiskDegraded { tenant: r.tenant });
                    }
                }
            }
            _ => {
                self.stats.stale_cqes += 1;
                self.try_finish_quiesce(idx, events);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{Outcome, Rng, SimIo};
    use crate::slot::{CoreControl, SlotReadError, Visibility};

    const W: usize = 512; // 4 KiB extents
    const DATA: usize = IMAGE_HEADER_WORDS; // first application word

    fn cfg() -> LifecycleConfig {
        LifecycleConfig {
            low_free: 2,
            high_free: 3,
            max_flush_inflight: 2,
            max_retries: 3,
            epoch: 1,
        }
    }
    /// Tenant t owns blocks [2 + 2t, 2 + 2t + 2): copy A then copy B (block 0 unused).
    fn loc(t: u32) -> ExtentLoc {
        ExtentLoc::new(0, 2 + 2 * t as u64, false)
    }
    fn value(arena: &SlotArena<W>, h: SlotHandle, t: u32) -> Result<u64, SlotReadError> {
        arena
            .read(h, t, (t) as u128 + 1, Visibility::Applied, |v| v.word(DATA))
            .map(|s| s.value)
    }
    fn deliver<const W2: usize>(
        lc: &mut SlotLifecycle<'_, W2>,
        io: &mut SimIo,
        i: usize,
        o: Outcome,
    ) -> Vec<Event> {
        let (ud, res) = io.complete(i, o);
        let mut ev = Vec::new();
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { lc.on_cqe(ud, res, io, &mut ev) };
        ev
    }

    #[test]
    fn r5_l2_stale_and_duplicate_cqes_do_not_decrement_or_abort() {
        let arena = SlotArena::<W>::new_heap(0, 4, CoreControl::shared(1));
        let route = RouteTable::new(16);
        let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..4, cfg());
        let mut io = SimIo::new(1, 40 * 4096);
        let mut ev = Vec::new();
        let h = lc.admit_new(0, 1, loc(0), 1, 1, &mut ev).unwrap();
        assert!(lc.checkpoint(h, 1, &mut io));
        let (ud, res) = io.complete(0, Outcome::Ok);
        let decoded = UserData::decode(ud).unwrap();
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe {
            lc.on_cqe(
                UserData {
                    ticket: decoded.ticket + 1,
                    ..decoded
                }
                .encode(),
                res,
                &mut io,
                &mut ev,
            )
        };
        assert_eq!(lc.flushing(), 1);
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { lc.on_cqe(ud, res, &mut io, &mut ev) };
        assert_eq!(lc.flushing(), 0);
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { lc.on_cqe(ud, res, &mut io, &mut ev) };
        assert_eq!(lc.flushing(), 0);
        assert_eq!(lc.stats.stale_cqes, 2);
        lc.unpin(u32::MAX, &mut ev);
        assert_eq!(lc.stats.stale_cqes, 3);
    }

    #[test]
    fn slot_is_freed_only_after_its_checkpoint_cqe() {
        let arena = SlotArena::<W>::new_heap(0, 4, CoreControl::shared(1));
        let route = RouteTable::new(16);
        let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..4, cfg());
        let mut io = SimIo::new(1, 40 * 4096);
        let mut ev = Vec::new();
        let hs: Vec<_> = (0..4)
            .map(|t| {
                lc.admit_new(t, (t) as u128 + 1, loc(t), 1 + t as u64, 1, &mut ev)
                    .unwrap()
            })
            .collect();
        assert_eq!(
            lc.admit_new(9, (9) as u128 + 1, loc(9), 9, 1, &mut ev),
            Err(AdmitError::NoFreeSlot { flushing: 0 })
        );
        for (t, &h) in hs.iter().enumerate() {
            lc.mutate(h, 10 + t as u64, |w| w.set(DATA, 0xA0 + t as u64))
                .unwrap();
        }

        // WAL rule (I3): nothing is durable yet, so nothing may be written back.
        assert_eq!(lc.run_eviction(0, &mut io, &mut ev), 0);
        assert!(io.inflight.is_empty() && lc.stats.wal_blocked > 0);

        // Durable: flushes start, but no slot is free until a CQE arrives.
        assert_eq!(lc.run_eviction(100, &mut io, &mut ev), 2);
        assert_eq!(
            (io.inflight.len(), lc.free_slots(), lc.flushing()),
            (2, 0, 2)
        );
        assert_eq!(
            lc.admit_new(9, (9) as u128 + 1, loc(9), 20, 1, &mut ev),
            Err(AdmitError::NoFreeSlot { flushing: 2 }),
            "back-pressure while flushing"
        );
        assert!(ev.iter().all(|e| !matches!(e, Event::Freed { .. })));

        // Mutations against a flushing slot are deferred (I2: DMA source is frozen).
        let flushing: Vec<u32> = (0..4)
            .filter(|&i| matches!(lc.state(i), State::Flushing { .. }))
            .collect();
        let (fa, fb) = (flushing[0], flushing[1]);
        assert_eq!(
            lc.mutate(hs[fa as usize], 30, |w| w.set(DATA, 1)),
            Err(Gate::Defer)
        );

        // A failed write keeps the slot resident and dirty, and resumes the deferred op.
        let ia = io
            .find(|op| UserData::decode(op.user_data).unwrap().object == fa)
            .unwrap();
        let e = deliver(&mut lc, &mut io, ia, Outcome::Err(libc::EIO));
        assert!(e.contains(&Event::FlushFailed {
            tenant: fa,
            res: -libc::EIO,
            retries: 1
        }));
        assert!(e.contains(&Event::Resume {
            tenant: fa,
            handle: hs[fa as usize],
            deferred: 1
        }));
        assert!(matches!(
            lc.state(fa),
            State::Resident(Residency { dirty: true, .. })
        ));
        assert_eq!(value(&arena, hs[fa as usize], fa), Ok(0xA0 + fa as u64));

        // A short write is a failure too (the image would be torn).
        let ib = io
            .find(|op| UserData::decode(op.user_data).unwrap().object == fb)
            .unwrap();
        let op = io.inflight[ib];
        assert_eq!(
            op.offset,
            loc(fb).flipped().current_offset(4096),
            "I4: checkpoint goes to the other copy"
        );
        let e = deliver(&mut lc, &mut io, ib, Outcome::Short(2048));
        assert!(matches!(e[0], Event::FlushFailed { .. }));
        assert_eq!(
            route.lookup(fb).unwrap().1,
            loc(fb),
            "route still points at the old copy"
        );

        // Retry, succeed: Checkpointed -> Evicted -> Freed, in that order, only now.
        let e = loop {
            lc.run_eviction(100, &mut io, &mut ev);
            match io.find(|op| UserData::decode(op.user_data).unwrap().object == fb) {
                Some(ib) => break deliver(&mut lc, &mut io, ib, Outcome::Ok),
                None => {
                    ev.extend(deliver(&mut lc, &mut io, 0, Outcome::Err(libc::EIO)));
                }
            }
        };
        let pos = |p: fn(&Event) -> bool| e.iter().position(p).unwrap();
        assert!(
            pos(|e| matches!(e, Event::Checkpointed { .. }))
                < pos(|e| matches!(e, Event::Evicted { .. }))
        );
        assert!(
            pos(|e| matches!(e, Event::Evicted { .. })) < pos(|e| matches!(e, Event::Freed { .. }))
        );
        assert_eq!(
            route.lookup(fb).unwrap(),
            (SlotHandle::NULL, loc(fb).flipped())
        );
        let img = &io.files[0][op.offset as usize..op.offset as usize + 4096];
        let parsed =
            ExtentImage::validate(img, fb, (fb) as u128 + 1, 1).expect("valid image on disk");
        assert_eq!(parsed.checkpoint_lsn, 10 + fb as u64);
        assert_eq!(
            u64::from_le_bytes(img[DATA * 8..DATA * 8 + 8].try_into().unwrap()),
            0xA0 + fb as u64
        );

        // Stale handle after reuse: the old generation can never read the new tenant.
        let old = hs[fb as usize];
        let h9 = lc
            .admit_new(9, (9) as u128 + 1, loc(9), 40, 1, &mut ev)
            .unwrap();
        assert_eq!(h9.index(), old.index());
        assert_ne!(h9.generation(), old.generation());
        assert_eq!(value(&arena, old, fb), Err(SlotReadError::Reassigned));
        assert_eq!(value(&arena, h9, 9), Ok(0));
        assert_eq!(lc.gate(old), Gate::Stale);

        // Out-of-range CQE: counted, ignored.
        let mut e = Vec::new();
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe {
            lc.on_cqe(
                UserData {
                    op: Op::Checkpoint,
                    object: 77,
                    ticket: 1,
                }
                .encode(),
                4096,
                &mut io,
                &mut e,
            )
        };
        assert!(e.is_empty() && lc.stats.stale_cqes == 1);
        // SAFETY: `io` is never completed again; it is dropped with this test.
        unsafe { lc.release_device_references() };
    }

    #[test]
    fn load_validates_image_and_fails_closed_when_current_copy_is_corrupt() {
        let arena = SlotArena::<W>::new_heap(0, 2, CoreControl::shared(1));
        let route = RouteTable::new(16);
        let mut lc = SlotLifecycle::new(
            &arena,
            &route,
            0,
            0..2,
            LifecycleConfig {
                low_free: 2,
                high_free: 2,
                ..cfg()
            },
        );
        let mut io = SimIo::new(1, 40 * 4096);
        let mut ev = Vec::new();
        // Tenant 3: write two checkpoints (A/B), evict.
        let h = lc
            .admit_new(3, (3) as u128 + 1, loc(3), 1, 1, &mut ev)
            .unwrap();
        for (lsn, v) in [(5u64, 111u64), (6, 222)] {
            lc.mutate(h, lsn, |w| w.set(DATA, v)).unwrap();
            assert!(lc.checkpoint(h, lsn, &mut io));
            let e = deliver(&mut lc, &mut io, 0, Outcome::Ok);
            assert!(matches!(e[0], Event::Checkpointed { lsn: l, .. } if l == lsn));
        }
        // CLOCK: first pass clears `referenced`, second evicts the clean slot.
        lc.run_eviction(6, &mut io, &mut ev);
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Evicted { tenant: 3, .. }))
        );
        let (_, cur) = route.lookup(3).unwrap();
        assert_eq!(cur, loc(3), "two flips: back to copy A");

        // Normal load.
        let idx = lc
            .admit_load(
                3,
                (3) as u128 + 1,
                RecoveryBounds::clean(2, 1, 6),
                cur,
                1,
                false,
                &mut io,
            )
            .unwrap();
        assert_eq!(
            value(&arena, h, 3),
            Err(SlotReadError::Reassigned),
            "loading slot is unowned"
        );
        let e = deliver(&mut lc, &mut io, 0, Outcome::Ok);
        let Event::Loaded {
            handle,
            checkpoint_lsn,
            ..
        } = e[0]
        else {
            panic!("{e:?}")
        };
        assert_eq!((handle.index(), checkpoint_lsn), (idx, 6));
        assert_eq!(value(&arena, handle, 3), Ok(222));

        // Corrupt the exact clean image; normal reload must not substitute older B.
        lc.run_eviction(6, &mut io, &mut ev);
        lc.run_eviction(6, &mut io, &mut ev);
        assert_eq!(route.lookup(3).unwrap().0, SlotHandle::NULL);
        let a = cur.current_offset(4096) as usize;
        io.files[0][a + 100] ^= 0xFF;
        lc.admit_load(
            3,
            (3) as u128 + 1,
            RecoveryBounds::clean(2, 1, 6),
            cur,
            1,
            false,
            &mut io,
        )
        .unwrap();
        let events = deliver(&mut lc, &mut io, 0, Outcome::Ok);
        assert!(events.contains(&Event::NeedsRepair { tenant: 3 }));
        assert!(events.iter().all(|e| !matches!(e, Event::Loaded { .. })));
        assert_eq!(route.lookup(3).unwrap().0, SlotHandle::NULL);
        assert!(io.inflight.is_empty());
    }

    #[test]
    fn crash_recovery_load_picks_newest_valid_copy() {
        let arena = SlotArena::<W>::new_heap(0, 2, CoreControl::shared(1));
        let route = RouteTable::new(16);
        let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, cfg());
        let mut io = SimIo::new(1, 40 * 4096);
        let mut ev = Vec::new();
        let h = lc
            .admit_new(4, (4) as u128 + 1, loc(4), 1, 1, &mut ev)
            .unwrap();
        for (lsn, v) in [(5u64, 1u64), (6, 2), (7, 3)] {
            lc.mutate(h, lsn, |w| w.set(DATA, v)).unwrap();
            assert!(lc.checkpoint(h, lsn, &mut io));
            deliver(&mut lc, &mut io, 0, Outcome::Ok);
        }
        // Newest image (seq 3, lsn 7) is in copy B. Lose the route table: recover.
        let route2 = RouteTable::new(16);
        let arena2 = SlotArena::<W>::new_heap(0, 2, CoreControl::shared(1));
        let mut lc2 = SlotLifecycle::new(&arena2, &route2, 0, 0..2, cfg());
        lc2.admit_load(
            4,
            (4) as u128 + 1,
            RecoveryBounds {
                current_epoch: 2,
                expected_image_epoch: None,
                wal_start_lsn: 1,
                wal_end_lsn: 7,
            },
            loc(4),
            1,
            true,
            &mut io,
        )
        .unwrap();
        let mut e = Vec::new();
        for _ in 0..3 {
            if io.inflight.is_empty() {
                break;
            }
            e = deliver(&mut lc2, &mut io, 0, Outcome::Ok);
        }
        let Event::Loaded {
            handle,
            checkpoint_lsn: 7,
            ..
        } = e[0]
        else {
            panic!("{e:?}")
        };
        assert_eq!(value(&arena2, handle, 4), Ok(3));
        assert_eq!(route2.lookup(4).unwrap().1, loc(4).flipped());
    }

    /// Random interleavings: mutations, checkpoints, evictions, loads, CQEs completed in
    /// random order with errors and short writes. The simulator moves bytes at CQE time,
    /// so any slot reuse before its CQE would put foreign bytes into an image and fail
    /// the value check on reload. Internal assertions enforce I1..I5 on every step.
    #[test]
    fn randomized_interleavings_never_lose_or_mix_state() {
        const T: u32 = 24;
        const S: u32 = 6;
        for seed in 1..=if cfg!(miri) { 1 } else { 40 } {
            let arena = SlotArena::<W>::new_heap(0, S, CoreControl::shared(1));
            let route = RouteTable::new(T);
            let mut lc = SlotLifecycle::new(
                &arena,
                &route,
                0,
                0..S,
                LifecycleConfig {
                    low_free: 3,
                    high_free: 4,
                    max_flush_inflight: 3,
                    max_retries: 50,
                    epoch: 1,
                },
            );
            let mut io = SimIo::new(1, (2 + 2 * T as usize + 2) * 4096);
            let mut rng = Rng(0x5EED ^ seed);
            // Model: committed value per tenant, and whether it exists on disk.
            let mut model = vec![None::<u64>; T as usize];
            let mut loading = vec![false; T as usize];
            let mut checkpointed = vec![0; T as usize];
            let mut lsn = 1u64;
            let mut ev = Vec::new();
            for _ in 0..3000 {
                let t = rng.below(T as u64) as u32;
                let (h, _) = route.lookup(t).unwrap();
                match rng.below(10) {
                    0..=5 if !h.is_null() => {
                        lsn += 1;
                        let v = rng.next();
                        if lc.mutate(h, lsn, |w| w.set(DATA, v)).is_ok() {
                            model[t as usize] = Some(v);
                        }
                        assert_eq!(
                            value(&arena, h, t)
                                .ok()
                                .zip(model[t as usize])
                                .is_none_or(|(a, b)| a == b),
                            true
                        );
                    }
                    0..=5 if !loading[t as usize] => match model[t as usize] {
                        None => {
                            lsn += 1;
                            if lc
                                .admit_new(t, (t) as u128 + 1, loc(t), lsn, 1, &mut ev)
                                .is_ok()
                            {
                                model[t as usize] = Some(0);
                            }
                        }
                        Some(_) => {
                            let cur = route.lookup(t).unwrap().1;
                            if lc
                                .admit_load(
                                    t,
                                    (t) as u128 + 1,
                                    RecoveryBounds::clean(1, 1, checkpointed[t as usize]),
                                    cur,
                                    1,
                                    false,
                                    &mut io,
                                )
                                .is_ok()
                            {
                                loading[t as usize] = true;
                            }
                        }
                    },
                    6 | 7 => {
                        lc.run_eviction(lsn, &mut io, &mut ev);
                    }
                    _ => {}
                }
                for _ in 0..rng.below(3) {
                    if io.inflight.is_empty() {
                        break;
                    }
                    let i = rng.below(io.inflight.len() as u64) as usize;
                    let o = match rng.below(12) {
                        0 => Outcome::Err(libc::EIO),
                        1 => Outcome::Short(2048),
                        _ => Outcome::Ok,
                    };
                    let (ud, res) = io.complete(i, o);
                    /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
                    unsafe { lc.on_cqe(ud, res, &mut io, &mut ev) };
                }
                for e in ev.drain(..) {
                    match e {
                        Event::Checkpointed { tenant, lsn, .. } => {
                            checkpointed[tenant as usize] = lsn
                        }
                        Event::ReplayRequired {
                            handle,
                            through_lsn,
                            ..
                        } => {
                            lc.replay(handle, through_lsn, |_| ()).unwrap();
                            assert!(lc.finish_replay(handle, &mut Vec::new()));
                            let State::Resident(r) = lc.state(handle.index()) else {
                                panic!("replay did not finish")
                            };
                            loading[r.tenant as usize] = false;
                            assert_eq!(
                                value(&arena, handle, r.tenant).ok(),
                                model[r.tenant as usize]
                            );
                        }
                        Event::Loaded { tenant, handle, .. } => {
                            loading[tenant as usize] = false;
                            assert_eq!(
                                value(&arena, handle, tenant).ok(),
                                model[tenant as usize],
                                "seed {seed}: reload returned wrong state"
                            );
                        }
                        Event::LoadFailed { tenant, errno } => {
                            assert!(errno < 0, "seed {seed}: a checkpointed image vanished");
                            loading[tenant as usize] = false;
                        }
                        Event::Resume { tenant, handle, .. } => {
                            assert_eq!(value(&arena, handle, tenant).ok(), model[tenant as usize]);
                        }
                        _ => {}
                    }
                }
                // Free + flushing + resident + loading accounting never exceeds capacity.
                assert!(lc.free_slots() <= S);
            }
            assert!(
                lc.stats.evictions > 20 && lc.stats.flush_errors > 0 && lc.stats.deferred > 0,
                "seed {seed}: weak coverage {:?}",
                lc.stats
            );
            // SAFETY: the simulated device is dropped with this iteration; it never
            // completes the remaining ops.
            unsafe { lc.release_device_references() };
        }
    }
}
