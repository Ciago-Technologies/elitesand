//! Per-core group-commit log.
//!
//! Spec defect: every commit appends a 28-byte delta to the tenant's own extent with
//! `O_DIRECT`, i.e. one >= 4 KiB device write (+ FUA/flush) per transaction per tenant:
//! 4096 / 28 = 146x write amplification, random I/O across 1M extents, and a device
//! flush on the critical path of every commit.
//!
//! Fix: each reactor core owns one append-only log file. Records from all tenants on the
//! core are packed into 4 KiB-aligned registered buffers and written sequentially;
//! extents become checkpoints (see `lifecycle`). The flush policy is adaptive:
//!
//! * seal + submit when the active buffer reaches `min_batch_bytes`, or
//! * the oldest record in it is `max_delay_ns` old, or
//! * the core's mailboxes are drained (`idle`), so at low load latency is one device
//!   write; at high load, while `max_inflight` writes are outstanding, records keep
//!   accumulating: batch size grows with load automatically (classic group commit).
//!
//! Durability watermark: buffers can complete out of order (NVMe has no ordering
//! between commands). `durable_lsn` only advances over the *contiguous prefix* of
//! completed buffers, and only then are acknowledgements released and
//! `CoreControl::durable_lsn` published to readers.
//!
//! Buffer lifetime is CQE-gated exactly like slots: a buffer returns to the free list
//! only after its write CQE succeeded and every earlier buffer is durable.
//!
//! Layout: `[superblock A | superblock B | data ring of `capacity` bytes]`. Logical
//! offsets grow forever; physical = `data_offset + logical % capacity`; each record
//! carries `lap = logical / capacity` and the log `epoch` (+1 per open, persisted in the
//! superblock before the first append), which makes stale records from older laps or
//! from a crashed incarnation unambiguous during recovery.

use crate::envelope::{
    KIND_PAD, REC_HDR, RecHeader, crc32c, decode_record, encode_pad_in_place, encode_record,
    record_size,
};
use ciagomem_core::io::{IoBackend, Op, UserData};
use ciagomem_core::slot::{CoreControl, DurabilityPublisher, SlotArena};
use core::ptr::NonNull;
use std::collections::VecDeque;
use std::sync::Arc;

/// A fenced write may already be on disk; clients must reconcile rather than blindly retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AckOutcome {
    Indeterminate(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailMode {
    /// Every write ends on a block boundary (PAD record). Never rewrites a block that
    /// holds acknowledged records. Safe on any device.
    Pad,
    /// The partially filled last block is carried into the next write and rewritten.
    /// Requires power-fail atomicity >= `block` (NVMe AWUPF covering the block, e.g. a
    /// 4 KiB LBA format, or `statx` `stx_atomic_write_unit_min >= 4096`).
    Rewrite,
}

#[derive(Debug, Clone, Copy)]
pub struct LogConfig {
    pub block: usize,
    pub buf_bytes: usize,
    pub buffers: usize,
    pub max_inflight: usize,
    pub min_batch_bytes: usize,
    pub max_delay_ns: u64,
    /// Data ring bytes (multiple of `block`).
    pub capacity: u64,
    pub file: u16,
    pub tail: TailMode,
    pub max_retries: u8,
}

impl LogConfig {
    pub const fn data_offset(&self) -> u64 {
        2 * self.block as u64
    }
    pub fn validate(&self) {
        assert!(self.block.is_power_of_two() && self.block >= 512);
        assert!(self.buf_bytes % self.block == 0 && self.buf_bytes >= 2 * self.block);
        assert!(
            self.capacity % self.block as u64 == 0
                && self.capacity >= (self.buffers * self.buf_bytes) as u64
        );
        assert!(self.buffers >= 2 && self.max_inflight >= 1 && self.max_inflight < self.buffers);
        assert!(self.buffers < (1 << 24));
    }
}

impl Default for LogConfig {
    /// Production defaults for one reactor core on an enterprise NVMe.
    fn default() -> Self {
        LogConfig {
            block: 4096,
            buf_bytes: 256 * 1024,
            buffers: 8,
            max_inflight: 4,
            min_batch_bytes: 64 * 1024,
            max_delay_ns: 200_000,
            capacity: 1 << 30,
            file: 0,
            tail: TailMode::Pad,
            max_retries: 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogError {
    /// Superblock for this incarnation not yet durable.
    NotReady,
    /// No free buffer or ring space: back-pressure (stop draining mailboxes).
    Full,
    TooLarge,
    MismatchedControls,
    /// A log write failed permanently: the core must stop accepting writes.
    Fenced(i32),
}

struct AlignedBuf {
    ptr: NonNull<u8>,
    len: usize,
}
// SAFETY: plain owned memory.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    fn new(len: usize, align: usize) -> Self {
        let layout = std::alloc::Layout::from_size_align(len, align).unwrap();
        // SAFETY: non-zero size.
        let ptr = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout) })
            .unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        AlignedBuf { ptr, len }
    }
    fn slice_mut(&mut self) -> &mut [u8] {
        // SAFETY: owned allocation of `len` bytes; only called while no write is in flight.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
    fn slice(&self) -> &[u8] {
        // SAFETY: reads are allowed while the device reads the buffer.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: matches `new`. Buffers are dropped only with the log, after the
        // owner has drained all CQEs (see `GroupCommitLog::quiescent`).
        unsafe {
            std::alloc::dealloc(
                self.ptr.as_ptr(),
                std::alloc::Layout::from_size_align(self.len, 4096).unwrap(),
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BufState {
    Free,
    Filling,
    Sealed,
    InFlight,
    Done,
}

struct LogBuf {
    mem: AlignedBuf,
    state: BufState,
    logical: u64,
    limit: usize,
    used: usize,
    first_lsn: u64,
    last_lsn: u64,
    records: u32,
    first_record_logical: u64,
    acks: Vec<u64>,
    opened_ns: u64,
    submit_len: usize,
    written: usize,
    ticket: u32,
    retries: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Superblock {
    pub seq: u64,
    pub epoch: u32,
    pub start_logical: u64,
    pub start_lsn: u64,
}

const SB_MAGIC: u64 = u64::from_le_bytes(*b"COAGOSB1");

impl Superblock {
    pub fn encode(&self, block: &mut [u8]) {
        block.fill(0);
        block[0..8].copy_from_slice(&SB_MAGIC.to_le_bytes());
        block[8..16].copy_from_slice(&self.seq.to_le_bytes());
        block[16..20].copy_from_slice(&self.epoch.to_le_bytes());
        block[24..32].copy_from_slice(&self.start_logical.to_le_bytes());
        block[32..40].copy_from_slice(&self.start_lsn.to_le_bytes());
        let c = crc32c(&[&block[0..40]]);
        block[40..44].copy_from_slice(&c.to_le_bytes());
    }
    pub fn decode(block: &[u8]) -> Option<Superblock> {
        if u64::from_le_bytes(block[0..8].try_into().unwrap()) != SB_MAGIC
            || crc32c(&[&block[0..40]]) != u32::from_le_bytes(block[40..44].try_into().unwrap())
        {
            return None;
        }
        Some(Superblock {
            seq: u64::from_le_bytes(block[8..16].try_into().unwrap()),
            epoch: u32::from_le_bytes(block[16..20].try_into().unwrap()),
            start_logical: u64::from_le_bytes(block[24..32].try_into().unwrap()),
            start_lsn: u64::from_le_bytes(block[32..40].try_into().unwrap()),
        })
    }
    /// A/B placement: even seq in block 0, odd in block 1.
    pub fn file_offset(&self, block: usize) -> u64 {
        (self.seq % 2) * block as u64
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LogStats {
    pub records: u64,
    pub record_bytes: u64,
    pub payload_bytes: u64,
    pub batches: u64,
    pub device_bytes: u64,
    pub pad_records: u64,
    pub short_writes: u64,
    pub retries: u64,
    pub stale_cqes: u64,
}

impl LogStats {
    /// Device bytes per logical payload byte.
    pub fn write_amplification(&self) -> f64 {
        self.device_bytes as f64 / self.payload_bytes.max(1) as f64
    }
}

/// What a crash-recovery scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recovery {
    pub sb: Superblock,
    pub end_logical: u64,
    pub next_lsn: u64,
    pub max_epoch: u32,
    pub records: u64,
}

impl Recovery {
    pub fn fresh() -> Recovery {
        Recovery {
            sb: Superblock {
                seq: 0,
                epoch: 0,
                start_logical: 0,
                start_lsn: 1,
            },
            end_logical: 0,
            next_lsn: 1,
            max_epoch: 0,
            records: 0,
        }
    }
}

pub struct GroupCommitLog {
    binding: DurabilityPublisher,
    /// Set by `release_device_references`: the device holds no pointer into `bufs`.
    device_released: bool,
    cfg: LogConfig,
    bufs: Vec<LogBuf>,
    free: Vec<usize>,
    active: Option<usize>,
    pipeline: VecDeque<usize>,
    next_lsn: u64,
    durable_lsn: u64,
    next_logical: u64,
    truncate_logical: u64,
    epoch: u32,
    sb: Superblock,
    ready: bool,
    fenced: Option<i32>,
    carry: Vec<u8>,
    carry_logical: Option<u64>,
    lsn_index: VecDeque<(u64, u64)>,
    pub stats: LogStats,
}

impl core::fmt::Debug for GroupCommitLog {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GroupCommitLog")
            .field("epoch", &self.epoch)
            .field("next_lsn", &self.next_lsn)
            .field("durable_lsn", &self.durable_lsn)
            .field("pipeline", &self.pipeline.len())
            .finish()
    }
}

impl Drop for GroupCommitLog {
    /// R9-1: a submitted write still references its buffer. Without a completion to wait
    /// for, fence the core (no further durability can be published and reopen needs
    /// fresh controls) and retain the buffers until process exit.
    /// shortcut: reclaim abandoned buffers only when a reactor can cancel and reap I/O.
    fn drop(&mut self) {
        if !self.quiescent() && !self.device_released {
            self.binding.control().fence();
            core::mem::forget(core::mem::take(&mut self.bufs));
        }
    }
}

impl GroupCommitLog {
    /// Open after recovery. The returned superblock (new epoch) must be made durable and
    /// then passed to `superblock_durable` before the first append.
    pub fn open(
        cfg: LogConfig,
        rec: &Recovery,
        controls: Arc<[CoreControl]>,
        core_id: u8,
    ) -> (GroupCommitLog, Superblock) {
        cfg.validate();
        assert!((core_id as usize) < controls.len() && !controls[core_id as usize].is_fenced());
        // Own the claim before fallible initialization so any unwind releases it.
        let binding = DurabilityPublisher::claim(controls, core_id)
            .unwrap_or_else(|| panic!("core {core_id} already has a bound log"));
        let core = binding.control();
        let recovered_end = rec
            .next_lsn
            .checked_sub(1)
            .expect("recovered next LSN must be nonzero");
        assert!(
            core.durable() <= recovered_end,
            "core {core_id} watermark {} exceeds recovered end {recovered_end}",
            core.durable()
        );
        let epoch = rec
            .max_epoch
            .max(rec.sb.epoch)
            .checked_add(1)
            .expect("log epoch exhausted");
        let sb = Superblock {
            seq: rec
                .sb
                .seq
                .checked_add(1)
                .expect("superblock sequence exhausted"),
            epoch,
            start_logical: rec.sb.start_logical,
            start_lsn: rec.sb.start_lsn,
        };
        let bufs = (0..cfg.buffers)
            .map(|_| LogBuf {
                mem: AlignedBuf::new(cfg.buf_bytes, 4096.max(cfg.block)),
                state: BufState::Free,
                logical: 0,
                limit: 0,
                used: 0,
                first_lsn: 0,
                last_lsn: 0,
                records: 0,
                first_record_logical: 0,
                acks: Vec::with_capacity(cfg.buf_bytes / 64),
                opened_ns: 0,
                submit_len: 0,
                written: 0,
                ticket: 0,
                retries: 0,
            })
            .collect();
        let log = GroupCommitLog {
            binding,
            device_released: false,
            cfg,
            bufs,
            free: (0..cfg.buffers).rev().collect(),
            active: None,
            pipeline: VecDeque::with_capacity(cfg.buffers),
            next_lsn: rec.next_lsn,
            durable_lsn: recovered_end,
            next_logical: rec.end_logical.next_multiple_of(cfg.block as u64),
            truncate_logical: rec.sb.start_logical,
            epoch,
            sb,
            ready: false,
            fenced: None,
            carry: Vec::with_capacity(cfg.block),
            carry_logical: None,
            lsn_index: VecDeque::new(),
            stats: LogStats::default(),
        };
        // Truncation point for "everything recovered is checkpointed": without it a
        // ring that recovery left nearly full could never reclaim space.
        let mut log = log;
        log.lsn_index.push_back((log.next_lsn, log.next_logical));
        (log, sb)
    }

    pub fn validate_arena<const W: usize>(&self, arena: &SlotArena<W>) -> Result<(), LogError> {
        if Arc::ptr_eq(self.binding.controls(), arena.controls()) {
            Ok(())
        } else {
            Err(LogError::MismatchedControls)
        }
    }
    pub fn superblock_durable(&mut self, sb: &Superblock) {
        assert_eq!(sb.epoch, self.epoch);
        self.sb = *sb;
        self.truncate_logical = self.truncate_logical.max(sb.start_logical);
        self.ready = true;
        self.binding.publish(self.durable_lsn);
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }
    pub fn durable_lsn(&self) -> u64 {
        self.durable_lsn
    }
    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }
    /// Registered-buffer regions (for `UringIo::new`).
    pub fn buffer_regions(&self) -> Vec<(*mut u8, usize)> {
        self.bufs
            .iter()
            .map(|b| (b.mem.ptr.as_ptr(), b.mem.len))
            .collect()
    }
    /// Declare that the device no longer references any submitted buffer, so a drop
    /// while writes are nominally in flight may free them. Writes stay unacknowledged.
    ///
    /// # Safety
    /// Every submitted write must have been cancelled and reaped, or the device/ring
    /// destroyed after its operations terminated (e.g. `SimIo::power_loss`).
    pub unsafe fn release_device_references(&mut self) {
        self.device_released = true;
    }
    /// True when no write is in flight (safe to drop the log).
    pub fn quiescent(&self) -> bool {
        self.bufs.iter().all(|b| b.state != BufState::InFlight)
    }
    fn in_pipeline_not_done(&self) -> usize {
        self.pipeline
            .iter()
            .filter(|&&i| self.bufs[i].state != BufState::Done)
            .count()
    }
    fn phys(&self, logical: u64) -> u64 {
        self.cfg.data_offset() + logical % self.cfg.capacity
    }

    fn open_buf(&mut self, now_ns: u64) -> Result<usize, LogError> {
        let block = self.cfg.block as u64;
        let (start, carry) = match self.carry_logical.take() {
            Some(l) => (l, true),
            None => (self.next_logical, false),
        };
        let to_wrap = self.cfg.capacity - start % self.cfg.capacity;
        let limit = (self.cfg.buf_bytes as u64).min(to_wrap) as usize;
        if start + limit as u64 - self.truncate_logical > self.cfg.capacity {
            if carry {
                self.carry_logical = Some(start);
            }
            return Err(LogError::Full);
        }
        let Some(i) = self.free.pop() else {
            if carry {
                self.carry_logical = Some(start);
            }
            return Err(LogError::Full);
        };
        debug_assert!(start % block == 0);
        let b = &mut self.bufs[i];
        debug_assert_eq!(b.state, BufState::Free);
        b.state = BufState::Filling;
        b.logical = start;
        b.limit = limit;
        b.used = 0;
        b.records = 0;
        b.opened_ns = now_ns;
        b.written = 0;
        b.retries = 0;
        if carry {
            let n = self.carry.len();
            b.mem.slice_mut()[..n].copy_from_slice(&self.carry);
            b.used = n;
        }
        self.active = Some(i);
        Ok(i)
    }

    /// Never start a record in the last `REC_HDR - 1` bytes of a block (keeps the
    /// recovery scanner's skip rule unambiguous).
    fn align_for_record(&mut self, i: usize) {
        let block = self.cfg.block;
        let b = &mut self.bufs[i];
        let rem = block - (b.used % block);
        if rem < REC_HDR && rem != block {
            let u = b.used;
            b.mem.slice_mut()[u..u + rem].fill(0);
            b.used += rem;
        }
    }

    fn write_record(&mut self, i: usize, h: RecHeader, payload: &[u8]) {
        let b = &mut self.bufs[i];
        let logical = b.logical + b.used as u64;
        let u = b.used;
        let n = encode_record(&mut b.mem.slice_mut()[u..], &h, payload);
        b.used += n;
        if b.records == 0 {
            b.first_lsn = h.lsn;
            b.first_record_logical = logical;
        }
        b.last_lsn = h.lsn;
        b.records += 1;
        self.stats.records += 1;
        self.stats.record_bytes += n as u64;
    }

    /// PAD record filling exactly `total` bytes from the current position.
    fn write_pad(&mut self, i: usize, total: usize) {
        let h = self.header(i, KIND_PAD, 0, total - REC_HDR);
        self.next_lsn += 1;
        let b = &mut self.bufs[i];
        let logical = b.logical + b.used as u64;
        let u = b.used;
        encode_pad_in_place(&mut b.mem.slice_mut()[u..], &h, total);
        b.used += total;
        if b.records == 0 {
            b.first_lsn = h.lsn;
            b.first_record_logical = logical;
        }
        b.last_lsn = h.lsn;
        b.records += 1;
        self.stats.pad_records += 1;
    }

    fn header(&self, i: usize, kind: u8, tenant: u32, len: usize) -> RecHeader {
        let b = &self.bufs[i];
        let logical = b.logical + b.used as u64;
        RecHeader {
            kind,
            flags: 0,
            len: len as u32,
            lsn: self.next_lsn,
            tenant,
            lap: (logical / self.cfg.capacity) as u32,
            epoch: self.epoch,
        }
    }

    /// Append one record. Returns its LSN; `ack` is released (in LSN order) once the
    /// record and everything before it are durable.
    pub fn append<I: IoBackend>(
        &mut self,
        tenant: u32,
        kind: u8,
        payload: &[u8],
        ack: Option<u64>,
        now_ns: u64,
        io: &mut I,
    ) -> Result<u64, LogError> {
        if let Some(e) = self.fenced {
            return Err(LogError::Fenced(e));
        }
        if !self.ready {
            return Err(LogError::NotReady);
        }
        let size = record_size(payload.len());
        if size + 2 * REC_HDR + self.cfg.block > self.cfg.buf_bytes {
            return Err(LogError::TooLarge);
        }
        let i = loop {
            let i = match self.active {
                Some(i) => i,
                None => self.open_buf(now_ns)?,
            };
            self.align_for_record(i);
            let b = &self.bufs[i];
            if b.used + size <= b.limit {
                break i;
            }
            // Does not fit. If this buffer ends exactly at the ring wrap, fill it to the
            // wrap with a PAD record so the next record starts the next lap cleanly.
            if (b.logical + b.limit as u64) % self.cfg.capacity == 0 && b.used < b.limit {
                let rem = b.limit - b.used;
                debug_assert!(rem >= REC_HDR && rem % 8 == 0);
                self.write_pad(i, rem);
            }
            self.seal(io);
        };
        let h = self.header(i, kind, tenant, payload.len());
        self.next_lsn += 1;
        self.write_record(i, h, payload);
        self.stats.payload_bytes += payload.len() as u64;
        if let Some(a) = ack {
            self.bufs[i].acks.push(a);
        }
        Ok(h.lsn)
    }

    /// Close the active buffer and queue it for submission.
    fn seal<I: IoBackend>(&mut self, io: &mut I) {
        let Some(i) = self.active.take() else { return };
        let block = self.cfg.block;
        let used = self.bufs[i].used;
        let rem = used.next_multiple_of(block) - used;
        let end_logical;
        match self.cfg.tail {
            TailMode::Pad => {
                if rem >= REC_HDR {
                    self.write_pad(i, rem);
                } else if rem > 0 {
                    self.bufs[i].mem.slice_mut()[used..used + rem].fill(0);
                    self.bufs[i].used += rem;
                }
                end_logical = self.bufs[i].logical + self.bufs[i].used as u64;
            }
            TailMode::Rewrite => {
                let at_wrap = (self.bufs[i].logical + used.next_multiple_of(block) as u64)
                    % self.cfg.capacity
                    == 0;
                if at_wrap && rem >= REC_HDR {
                    // The last block of a lap is never carried: close it like Pad mode.
                    self.write_pad(i, rem);
                }
                let b = &mut self.bufs[i];
                let used = b.used;
                let rem = used.next_multiple_of(block) - used;
                b.mem.slice_mut()[used..used + rem].fill(0);
                let tail_start = used / block * block;
                if rem > 0 && !at_wrap {
                    self.carry.clear();
                    self.carry
                        .extend_from_slice(&b.mem.slice()[tail_start..used]);
                    self.carry_logical = Some(b.logical + tail_start as u64);
                }
                end_logical = b.logical + used.next_multiple_of(block) as u64;
            }
        }
        let b = &mut self.bufs[i];
        b.submit_len = b.used.next_multiple_of(block);
        b.state = BufState::Sealed;
        self.next_logical = end_logical;
        self.pipeline.push_back(i);
        self.stats.batches += 1;
        self.try_submit(io);
    }

    fn overlaps_inflight(&self, i: usize) -> bool {
        let b = &self.bufs[i];
        let (s, e) = (b.logical, b.logical + b.submit_len as u64);
        self.pipeline.iter().any(|&j| {
            let o = &self.bufs[j];
            j != i
                && o.state == BufState::InFlight
                && s < o.logical + o.submit_len as u64
                && o.logical < e
        })
    }

    fn try_submit<I: IoBackend>(&mut self, io: &mut I) {
        let inflight = self
            .pipeline
            .iter()
            .filter(|&&j| self.bufs[j].state == BufState::InFlight)
            .count();
        let mut budget = self.cfg.max_inflight.saturating_sub(inflight);
        for k in 0..self.pipeline.len() {
            let i = self.pipeline[k];
            match self.bufs[i].state {
                BufState::Sealed => {}
                BufState::InFlight | BufState::Done => continue,
                _ => unreachable!(),
            }
            // FIFO: a buffer that cannot go yet blocks those behind it (keeps the
            // rewrite-tail ordering trivially correct).
            if budget == 0 || self.overlaps_inflight(i) {
                return;
            }
            let b = &self.bufs[i];
            let ticket = b.ticket.wrapping_add(1);
            let ud = UserData {
                op: Op::LogWrite,
                object: i as u32,
                ticket,
            }
            .encode();
            let off = self.phys(b.logical) + b.written as u64;
            let len = (b.submit_len - b.written) as u32;
            let was_released = self.device_released;
            self.device_released = false;
            let b = &mut self.bufs[i];
            b.ticket = ticket;
            b.state = BufState::InFlight;
            // SAFETY: the buffer is frozen: no CPU writes until it is recycled after its
            // CQE. It is not freed before the log is quiescent.
            let r = unsafe {
                io.write_at(
                    ud,
                    self.cfg.file,
                    b.mem.ptr.as_ptr().add(b.written),
                    len,
                    off,
                    true,
                )
            };
            if r.is_err() {
                self.bufs[i].state = BufState::Sealed;
                self.device_released = was_released;
                return;
            }
            self.stats.device_bytes += len as u64;
            budget -= 1;
        }
    }

    /// Call once per reactor iteration after draining mailboxes.
    pub fn poll<I: IoBackend>(&mut self, now_ns: u64, idle: bool, io: &mut I) {
        if self.fenced.is_some() {
            return;
        }
        if let Some(i) = self.active {
            let b = &self.bufs[i];
            let not_done = self.in_pipeline_not_done();
            if b.records > 0 && not_done < self.cfg.max_inflight {
                let age = now_ns.saturating_sub(b.opened_ns);
                // Big enough or old enough: pipeline it. Idle with the device idle: flush
                // now for latency. Otherwise keep batching behind the in-flight write.
                if b.used >= self.cfg.min_batch_bytes
                    || age >= self.cfg.max_delay_ns
                    || (idle && not_done == 0)
                {
                    self.seal(io);
                }
            }
        }
        self.try_submit(io);
    }

    /// Feed every `Op::LogWrite` CQE here. Durable acknowledgements (in LSN order) are
    /// appended to `acks`; if the log fences, every pending acknowledgement is moved to
    /// `indeterminate` and `Err(Fenced)` is returned. No promise of rollback is made.
    ///
    /// # Safety
    /// A matching token/result must be a genuine completed write owned by this log;
    /// the backend must no longer access its buffer. Do not forge a completion for
    /// pending I/O, which would allow that buffer to be freed while still in use.
    /// ```compile_fail
    /// use ciagomem_wal::gclog::GroupCommitLog;
    /// use ciagomem_core::io::IoBackend;
    /// fn forged(owner:&mut GroupCommitLog,io:&mut impl IoBackend) {
    ///     let _=owner.on_cqe(0,4096,io,&mut Vec::new(),&mut Vec::new());
    /// }
    /// ```
    pub unsafe fn on_cqe<I: IoBackend>(
        &mut self,
        user_data: u64,
        res: i32,
        io: &mut I,
        acks: &mut Vec<u64>,
        indeterminate: &mut Vec<AckOutcome>,
    ) -> Result<(), LogError> {
        if self.binding.control().is_fenced() && self.fenced.is_none() {
            self.fence(libc::EIO, indeterminate);
        }
        if let Some(errno) = self.fenced {
            self.binding.control().fence();
            return Err(LogError::Fenced(errno));
        }
        let Some(ud) = UserData::decode(user_data) else {
            return Ok(());
        };
        let i = ud.object as usize;
        if i >= self.bufs.len()
            || self.bufs[i].state != BufState::InFlight
            || self.bufs[i].ticket != ud.ticket
        {
            self.stats.stale_cqes += 1;
            return Ok(());
        }
        let block = self.cfg.block;
        let b = &mut self.bufs[i];
        let want = b.submit_len - b.written;
        if res >= 0 && res as usize == want {
            b.written = b.submit_len;
            b.state = BufState::Done;
        } else if res > 0 && (res as usize) < want {
            // Short write: keep the block-aligned prefix, resubmit the rest.
            b.written += res as usize / block * block;
            b.state = BufState::Sealed;
            self.stats.short_writes += 1;
        } else {
            b.retries += 1;
            self.stats.retries += 1;
            if b.retries > self.cfg.max_retries {
                let errno = if res < 0 { res } else { -libc::EIO };
                self.binding.control().fence();
                self.fence(errno, indeterminate);
                return Err(LogError::Fenced(errno));
            }
            b.state = BufState::Sealed;
        }
        self.advance(acks);
        self.try_submit(io);
        Ok(())
    }

    fn advance(&mut self, acks: &mut Vec<u64>) {
        let mut moved = false;
        while let Some(&i) = self.pipeline.front() {
            if self.bufs[i].state != BufState::Done {
                break;
            }
            self.pipeline.pop_front();
            let b = &mut self.bufs[i];
            if b.records > 0 {
                self.durable_lsn = b.last_lsn;
                self.lsn_index
                    .push_back((b.first_lsn, b.first_record_logical));
            }
            acks.append(&mut b.acks);
            b.state = BufState::Free;
            self.free.push(i);
            moved = true;
        }
        if moved {
            self.binding.publish(self.durable_lsn);
        }
    }

    fn fence(&mut self, errno: i32, indeterminate: &mut Vec<AckOutcome>) {
        self.fenced = Some(errno);
        for &i in &self.pipeline {
            indeterminate.extend(self.bufs[i].acks.drain(..).map(AckOutcome::Indeterminate));
        }
        if let Some(i) = self.active {
            indeterminate.extend(self.bufs[i].acks.drain(..).map(AckOutcome::Indeterminate));
        }
    }

    /// Records with LSN < `min_needed_lsn` are folded into durable checkpoints. Returns
    /// the superblock that moves the recovery start forward; persist it, then call
    /// `superblock_durable` to actually reclaim ring space.
    pub fn truncation(&mut self, min_needed_lsn: u64) -> Option<Superblock> {
        let mut pick = None;
        while let Some(&(lsn, logical)) = self.lsn_index.front() {
            if lsn > min_needed_lsn {
                break;
            }
            pick = Some((lsn, logical));
            self.lsn_index.pop_front();
        }
        let (lsn, logical) = pick?;
        // Keep the chosen point so a later call can still start there.
        self.lsn_index.push_front((lsn, logical));
        (logical > self.sb.start_logical).then(|| Superblock {
            seq: self.sb.seq + 1,
            epoch: self.epoch,
            start_logical: logical,
            start_lsn: lsn,
        })
    }
}

/// Crash-recovery scan. `read(phys_offset, buf)` must fill `buf` (block aligned).
/// `on_record` sees every non-pad record in LSN order.
pub fn recover(
    cfg: &LogConfig,
    read: &mut dyn FnMut(u64, &mut [u8]) -> std::io::Result<()>,
    on_record: &mut dyn FnMut(&RecHeader, &[u8]),
) -> std::io::Result<Recovery> {
    cfg.validate();
    let block = cfg.block;
    let mut sbuf = vec![0u8; 2 * block];
    read(0, &mut sbuf)?;
    let sb = [
        Superblock::decode(&sbuf[..block]),
        Superblock::decode(&sbuf[block..]),
    ]
    .into_iter()
    .flatten()
    .max_by_key(|s| s.seq);
    let Some(sb) = sb else {
        return Ok(Recovery::fresh());
    };

    let chunk = cfg.buf_bytes;
    let mut win = vec![0u8; 2 * chunk];
    let mut win_start = u64::MAX; // logical offset of win[0]
    let mut win_len = 0usize;
    let mut fill = |pos: u64,
                    need: usize,
                    win: &mut Vec<u8>,
                    win_start: &mut u64,
                    win_len: &mut usize|
     -> std::io::Result<bool> {
        if *win_start != u64::MAX
            && pos >= *win_start
            && pos + need as u64 <= *win_start + *win_len as u64
        {
            return Ok(true);
        }
        let start = pos / block as u64 * block as u64;
        let to_wrap = cfg.capacity - start % cfg.capacity;
        let len = (win.len() as u64).min(to_wrap) as usize;
        if (pos - start) as usize + need > len {
            return Ok(false);
        }
        read(cfg.data_offset() + start % cfg.capacity, &mut win[..len])?;
        *win_start = start;
        *win_len = len;
        Ok(true)
    };

    let mut pos = sb.start_logical;
    let mut expect = sb.start_lsn;
    let mut last_epoch: Option<u32> = None;
    let mut max_epoch = sb.epoch;
    let mut records = 0u64;

    // Try to accept a record at `at`; returns its size.
    let mut try_at = |at: u64,
                      expect: u64,
                      last_epoch: Option<u32>,
                      win: &mut Vec<u8>,
                      ws: &mut u64,
                      wl: &mut usize|
     -> std::io::Result<Option<(RecHeader, usize)>> {
        if !fill(at, REC_HDR, win, ws, wl)? {
            return Ok(None);
        }
        let off = (at - *ws) as usize;
        let len = u32::from_le_bytes(win[off + 4..off + 8].try_into().unwrap()) as usize;
        let size = record_size(len);
        if size > chunk || !fill(at, size, win, ws, wl)? {
            return Ok(None);
        }
        let off = (at - *ws) as usize;
        let Some((h, _)) = decode_record(&win[off..off + size]) else {
            return Ok(None);
        };
        let lap_ok = h.lap as u64 == at / cfg.capacity;
        let epoch_ok = h.epoch <= sb.epoch
            && match last_epoch {
                None => true,
                Some(e) => h.epoch == e || (h.epoch > e && at % block as u64 == 0),
            };
        Ok((h.lsn == expect && lap_ok && epoch_ok).then_some((h, size)))
    };

    loop {
        let rem = block as u64 - pos % block as u64;
        if rem < REC_HDR as u64 && rem != block as u64 {
            pos += rem;
        }
        let mut hit = try_at(
            pos,
            expect,
            last_epoch,
            &mut win,
            &mut win_start,
            &mut win_len,
        )?;
        if hit.is_none() && pos % block as u64 != 0 {
            // A hole is legal only up to the next block boundary and only where a new
            // incarnation (higher epoch) starts.
            let b = pos.next_multiple_of(block as u64);
            if let Some((h, s)) = try_at(
                b,
                expect,
                last_epoch,
                &mut win,
                &mut win_start,
                &mut win_len,
            )? {
                if last_epoch.is_some_and(|e| h.epoch > e) {
                    pos = b;
                    hit = Some((h, s));
                }
            }
        }
        let Some((h, size)) = hit else { break };
        let off = (pos - win_start) as usize;
        if h.kind != KIND_PAD {
            on_record(&h, &win[off + REC_HDR..off + REC_HDR + h.len as usize]);
            records += 1;
        }
        last_epoch = Some(h.epoch);
        max_epoch = max_epoch.max(h.epoch);
        expect += 1;
        pos += size as u64;
    }
    Ok(Recovery {
        sb,
        end_logical: pos,
        next_lsn: expect,
        max_epoch,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::KIND_DATA;
    use ciagomem_core::io::{Outcome, Rng, SimIo};
    use std::collections::BTreeMap;

    fn cfg(tail: TailMode) -> LogConfig {
        LogConfig {
            block: 4096,
            buf_bytes: 16 * 1024,
            buffers: 4,
            max_inflight: 3,
            min_batch_bytes: 8 * 1024,
            max_delay_ns: 5_000,
            capacity: 64 * 1024,
            file: 0,
            tail,
            max_retries: 3,
        }
    }

    fn write_sb(io: &mut SimIo, cfg: &LogConfig, sb: &Superblock) {
        let off = sb.file_offset(cfg.block) as usize;
        sb.encode(&mut io.files[0][off..off + cfg.block]);
    }

    fn payload(seed: u64, rng: &mut Rng) -> Vec<u8> {
        let n = 1 + rng.below(700) as usize;
        (0..n)
            .map(|i| (seed.wrapping_mul(31) as usize + i) as u8)
            .collect()
    }

    struct Model {
        /// lsn -> (tenant, payload) for every appended data record.
        appended: BTreeMap<u64, (u32, Vec<u8>)>,
        /// ack id -> lsn
        ack_lsn: BTreeMap<u64, u64>,
        acked: Vec<u64>,
        next_ack: u64,
    }

    /// Random workload with random CQE order, short writes and transient errors.
    /// Returns when `steps` are done, leaving I/O in flight for a power-loss.
    fn run(
        log: &mut GroupCommitLog,
        io: &mut SimIo,
        core: &CoreControl,
        m: &mut Model,
        rng: &mut Rng,
        steps: usize,
        cfg: &LogConfig,
        now: &mut u64,
    ) {
        let mut acks = Vec::new();
        let mut failed = Vec::new();
        for step in 0..steps {
            *now += 1_000;
            for _ in 0..rng.below(6) {
                let p = payload(m.next_ack, rng);
                let tenant = rng.below(1000) as u32;
                match log.append(tenant, KIND_DATA, &p, Some(m.next_ack), *now, io) {
                    Ok(lsn) => {
                        m.appended.insert(lsn, (tenant, p));
                        m.ack_lsn.insert(m.next_ack, lsn);
                        m.next_ack += 1;
                    }
                    Err(LogError::Full) => break,
                    Err(e) => panic!("{e:?}"),
                }
            }
            log.poll(*now, rng.below(3) == 0, io);
            for _ in 0..rng.below(3) {
                if io.inflight.is_empty() {
                    break;
                }
                let i = rng.below(io.inflight.len() as u64) as usize;
                let len = io.inflight[i].len;
                let outcome = match rng.below(40) {
                    0 => Outcome::Err(libc::EIO),
                    1 | 2 => Outcome::Short(rng.below(len as u64) as u32),
                    _ => Outcome::Ok,
                };
                let (ud, res) = io.complete(i, outcome);
                /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
                unsafe { log.on_cqe(ud, res, io, &mut acks, &mut failed) }.unwrap();
                for a in acks.drain(..) {
                    let lsn = m.ack_lsn[&a];
                    assert!(
                        lsn <= log.durable_lsn() && lsn <= core.durable(),
                        "ack {a} (lsn {lsn}) before durability"
                    );
                    assert!(m.acked.last().is_none_or(|&p| p < a), "acks out of order");
                    m.acked.push(a);
                }
            }
            assert!(failed.is_empty());
            if step % 25 == 0 {
                // Checkpoints cover everything durable: let the ring reclaim space.
                if let Some(sb) = log.truncation(log.durable_lsn() + 1) {
                    write_sb(io, cfg, &sb);
                    log.superblock_durable(&sb);
                }
            }
        }
    }

    fn recover_and_check(io: &SimIo, cfg: &LogConfig, m: &Model) -> Recovery {
        let file = io.files[0].clone();
        let mut seen: Vec<(u64, u32, Vec<u8>)> = Vec::new();
        let rec = recover(
            cfg,
            &mut |off, buf| {
                buf.copy_from_slice(&file[off as usize..off as usize + buf.len()]);
                Ok(())
            },
            &mut |h, p| seen.push((h.lsn, h.tenant, p.to_vec())),
        )
        .unwrap();
        for (lsn, tenant, p) in &seen {
            let (t, want) = m
                .appended
                .get(lsn)
                .expect("recovered a record that was never appended");
            assert_eq!((t, want), (tenant, p), "payload mismatch at lsn {lsn}");
        }
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0));
        let recovered: std::collections::HashSet<u64> = seen.iter().map(|s| s.0).collect();
        for a in &m.acked {
            let lsn = m.ack_lsn[a];
            if lsn >= rec.sb.start_lsn {
                assert!(
                    recovered.contains(&lsn),
                    "acknowledged lsn {lsn} lost (start {})",
                    rec.sb.start_lsn
                );
            }
        }
        rec
    }

    fn crash_cycle(tail: TailMode, seed: u64) {
        let cfg = cfg(tail);
        let mut io = SimIo::new(1, (cfg.data_offset() + cfg.capacity) as usize);
        let controls = CoreControl::shared(1);
        let core = &controls[0];
        let mut rng = Rng(seed);
        let mut m = Model {
            appended: BTreeMap::new(),
            ack_lsn: BTreeMap::new(),
            acked: Vec::new(),
            next_ack: 1,
        };
        let mut now = 0;
        let mut rec = Recovery::fresh();
        for incarnation in 0..4 {
            let (mut log, sb) = GroupCommitLog::open(cfg, &rec, controls.clone(), 0);
            assert_eq!(
                log.append(1, KIND_DATA, b"x", None, now, &mut io),
                Err(LogError::NotReady)
            );
            write_sb(&mut io, &cfg, &sb);
            log.superblock_durable(&sb);
            assert_eq!(log.epoch() as usize, incarnation + 1);
            run(
                &mut log, &mut io, core, &mut m, &mut rng, 400, &cfg, &mut now,
            );
            // Power loss: in-flight writes persist a random 4 KiB-granular prefix.
            let mut r2 = rng.clone();
            io.power_loss(|_, op| r2.below(op.len as u64 + 1) as u32);
            rng.next();
            rec = recover_and_check(&io, &cfg, &m);
            if std::env::var("GCLOG_TRACE").is_ok() {
                eprintln!(
                    "{tail:?} inc {incarnation}: acked {} next_logical {} rec {:?} stats {:?}",
                    m.acked.len(),
                    log.next_logical,
                    rec,
                    log.stats
                );
            }
            // Records beyond the recovered end were never acknowledged; forget them.
            m.appended.retain(|&l, _| l < rec.next_lsn);
            let keep: Vec<u64> = m
                .ack_lsn
                .iter()
                .filter(|&(_, &l)| l >= rec.next_lsn)
                .map(|(&a, _)| a)
                .collect();
            for a in keep {
                m.ack_lsn.remove(&a);
            }
            // SAFETY: SimIo::power_loss removed every in-flight op above.
            unsafe { log.release_device_references() };
            drop(log);
        }
        assert!(
            m.acked.len() > 2500,
            "workload too small: {}",
            m.acked.len()
        );
    }

    #[test]
    fn crash_recovery_keeps_every_ack_pad_mode() {
        for seed in 1..=if cfg!(miri) { 1 } else { 12 } {
            crash_cycle(TailMode::Pad, 0x9E37_79B9 ^ seed);
        }
    }

    #[test]
    fn crash_recovery_keeps_every_ack_rewrite_mode() {
        for seed in 1..=if cfg!(miri) { 1 } else { 12 } {
            crash_cycle(TailMode::Rewrite, 0xC0FF_EE00 ^ seed);
        }
    }

    #[test]
    fn out_of_order_cqes_release_acks_in_prefix_order() {
        let cfg = cfg(TailMode::Pad);
        let mut io = SimIo::new(1, (cfg.data_offset() + cfg.capacity) as usize);
        let controls = CoreControl::shared(1);
        let core = &controls[0];
        let (mut log, sb) = GroupCommitLog::open(cfg, &Recovery::fresh(), controls.clone(), 0);
        log.superblock_durable(&sb);
        // Three sealed batches.
        for b in 0..3u64 {
            log.append(7, KIND_DATA, &[b as u8; 100], Some(b), 0, &mut io)
                .unwrap();
            log.poll(1_000_000, true, &mut io); // idle + device idle only for the first
            log.poll(1_000_000, false, &mut io); // age >= max_delay seals the rest
        }
        assert_eq!(io.inflight.len(), 3);
        let (mut acks, mut failed) = (Vec::new(), Vec::new());
        // Complete the last two first: nothing may be acknowledged.
        for _ in 0..2 {
            let i = io.inflight.len() - 1;
            let (ud, res) = io.complete(i, Outcome::Ok);
            /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
            unsafe { log.on_cqe(ud, res, &mut io, &mut acks, &mut failed) }.unwrap();
        }
        assert!(acks.is_empty() && log.durable_lsn() == 0 && core.durable() == 0);
        let (ud, res) = io.complete(0, Outcome::Ok);
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { log.on_cqe(ud, res, &mut io, &mut acks, &mut failed) }.unwrap();
        assert_eq!(acks, vec![0, 1, 2]);
        assert_eq!(core.durable(), log.durable_lsn());
        // A duplicate/stale CQE is ignored.
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { log.on_cqe(ud, res, &mut io, &mut acks, &mut failed) }.unwrap();
        assert_eq!(log.stats.stale_cqes, 1);
        assert!(log.quiescent());
    }

    #[test]
    fn persistent_write_error_fences_and_fails_pending_acks() {
        let cfg = cfg(TailMode::Pad);
        let mut io = SimIo::new(1, (cfg.data_offset() + cfg.capacity) as usize);
        let controls = CoreControl::shared(1);
        let (mut log, sb) = GroupCommitLog::open(cfg, &Recovery::fresh(), controls.clone(), 0);
        log.superblock_durable(&sb);
        log.append(1, KIND_DATA, b"a", Some(10), 0, &mut io)
            .unwrap();
        log.poll(0, true, &mut io);
        log.append(1, KIND_DATA, b"b", Some(11), 0, &mut io)
            .unwrap();
        let (mut acks, mut failed) = (Vec::new(), Vec::new());
        let mut fenced = None;
        for _ in 0..=cfg.max_retries {
            let (ud, res) = io.complete(0, Outcome::Err(libc::EIO));
            if let Err(e) =
                /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
                unsafe { log.on_cqe(ud, res, &mut io, &mut acks, &mut failed) }
            {
                fenced = Some(e);
            }
        }
        assert_eq!(fenced, Some(LogError::Fenced(-libc::EIO)));
        assert!(acks.is_empty());
        failed.sort();
        assert_eq!(
            failed,
            vec![AckOutcome::Indeterminate(10), AckOutcome::Indeterminate(11)]
        );
        assert_eq!(
            log.append(1, KIND_DATA, b"c", None, 0, &mut io),
            Err(LogError::Fenced(-libc::EIO))
        );
    }

    #[test]
    fn group_commit_write_amplification_under_load() {
        let cfg = LogConfig {
            capacity: 1 << 26,
            ..LogConfig::default()
        };
        let mut io = SimIo::new(1, (cfg.data_offset() + cfg.capacity) as usize);
        let controls = CoreControl::shared(1);
        let (mut log, sb) = GroupCommitLog::open(cfg, &Recovery::fresh(), controls.clone(), 0);
        log.superblock_durable(&sb);
        let (mut acks, mut failed) = (Vec::new(), Vec::new());
        // 28-byte deltas (the spec's example), 10k commits, device completes 1 op/tick.
        let mut now = 0;
        let mut n = 0;
        while n < 10_000 {
            now += 1_000;
            for _ in 0..16 {
                if log
                    .append(
                        n as u32 % 4096,
                        crate::envelope::KIND_DATA,
                        &[0xAB; 28],
                        Some(n),
                        now,
                        &mut io,
                    )
                    .is_ok()
                {
                    n += 1;
                }
            }
            log.poll(now, false, &mut io);
            if !io.inflight.is_empty() {
                let (ud, res) = io.complete(0, Outcome::Ok);
                /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
                unsafe { log.on_cqe(ud, res, &mut io, &mut acks, &mut failed) }.unwrap();
            }
        }
        let wa = log.stats.write_amplification();
        let per_tenant = 4096.0 / 28.0;
        assert!(wa < 3.0, "WA {wa}");
        assert!(per_tenant / wa > 50.0);
        assert!(
            log.stats.batches * 20 < log.stats.records,
            "batches {} records {}",
            log.stats.batches,
            log.stats.records
        );
    }
}
