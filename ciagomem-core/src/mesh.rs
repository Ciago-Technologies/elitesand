//! N x N SPSC core mesh.
//!
//! Spec defect: one `SpscActorMailbox` per core, pushed to by every reactor core and by
//! the Calvin sequencer, using a non-atomic "load tail, write slot, store tail" sequence.
//! Two producers can claim the same slot: messages are lost or duplicated and the slot
//! write is a data race (UB).
//!
//! Fix: one ring per *(producer, consumer)* pair. Every ring has exactly one producer
//! endpoint and one consumer endpoint, enforced by the type system (endpoints are
//! `Send + !Sync + !Clone`), so the classic Lamport SPSC protocol is correct and needs
//! no RMW instruction per message.
//!
//! ```text
//!  producers P = reactor cores (0..C) + sequencer (C) + io/admin threads
//!  consumers C = reactor cores
//!
//!  InboundBlock[dst]  (allocated by / first-touched on dst's NUMA node)
//!  +-------------------------------------------------------------+
//!  | doorbell[ceil(P/64)] : CachePadded<AtomicU64>   bit src set |
//!  | parked               : CachePadded<AtomicU32>               |
//!  | ring[src = 0]   tail(128B) | head(128B) | slots[N] x T       |
//!  | ring[src = 1]   ...                                         |
//!  | ring[src = P-1]                                             |
//!  +-------------------------------------------------------------+
//! ```
//!
//! * Producer state that only the producer touches (`tail` shadow, cached `head`) lives
//!   in the endpoint, not in shared memory. The shared `tail` line is written once per
//!   `flush()` (batch), the shared `head` line once per consumer drain pass.
//! * Doorbell: on flush the producer sets bit `src` in the consumer's doorbell. The
//!   consumer `swap(0)`s doorbell words and drains only flagged lanes, so an idle 64-core
//!   mesh costs one cache line read per poll instead of 64.
//! * Parking: a consumer that wants to sleep in `io_uring_enter` publishes `parked = 1`,
//!   issues a SeqCst fence and re-checks the doorbell; a producer issues a SeqCst fence
//!   after ringing and reads `parked`. That is the Dekker pattern: at least one side sees
//!   the other, so no wake-up is lost. A needed wake-up is delivered as an
//!   `IORING_OP_MSG_RING` CQE into the consumer's ring (see `uring::post_wake`).
//!
//! Footprint for C = 16 reactors, P = 17 producers, N = 1024 slots of 64-byte `MeshMsg`:
//! 17 * 16 rings * (256 B + 64 KiB) = 17.07 MiB, fixed at boot, never grows.

use crate::CachePadded;
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::sync::atomic::{
    AtomicU32, AtomicU64,
    Ordering::{AcqRel, Acquire, Relaxed, Release, SeqCst},
    fence,
};
use std::sync::Arc;

/// 64-byte POD message: exactly one cache line per slot, no pointers into the producer's
/// heap (large payloads travel as offsets into a per-core arena with an ownership token
/// returned on the reverse lane).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[repr(C, align(64))]
pub struct MeshMsg {
    pub kind: u16,
    pub flags: u16,
    pub src_core: u16,
    /// Participant index of the receiving core within a multi-core tx (Calvin).
    pub participant: u8,
    /// Number of participant cores (Calvin), 1..=64.
    pub participants: u8,
    pub tx_seq: u64,
    pub tenant: u32,
    pub arg_len: u32,
    pub slot: u64,
    /// Offset of the argument blob in the producer core's arena.
    pub arg_off: u64,
    pub reply_token: u64,
    pub aux: [u64; 2],
}
const _: () = assert!(core::mem::size_of::<MeshMsg>() == 64);

struct Ring<T, const N: usize> {
    /// Written by the producer (Release), read by the consumer (Acquire).
    tail: CachePadded<AtomicU64>,
    /// Written by the consumer (Release), read by the producer (Acquire).
    head: CachePadded<AtomicU64>,
    slots: [UnsafeCell<MaybeUninit<T>>; N],
}

impl<T, const N: usize> Ring<T, N> {
    fn new() -> Self {
        Ring {
            tail: CachePadded(AtomicU64::new(0)),
            head: CachePadded(AtomicU64::new(0)),
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
        }
    }
    #[inline(always)]
    fn slot(&self, i: u64) -> *mut MaybeUninit<T> {
        self.slots[(i as usize) & (N - 1)].get()
    }
}

impl<T, const N: usize> Drop for Ring<T, N> {
    fn drop(&mut self) {
        let (mut h, t) = (*self.head.0.get_mut(), *self.tail.0.get_mut());
        while h != t {
            // SAFETY: published, unconsumed slots are initialized; we have `&mut self`.
            unsafe { (*self.slot(h)).assume_init_drop() };
            h += 1;
        }
    }
}

struct InboundBlock<T, const N: usize> {
    doorbell: Box<[CachePadded<AtomicU64>]>,
    parked: CachePadded<AtomicU32>,
    rings: Box<[Ring<T, N>]>,
}

// SAFETY: each ring slot is accessed by exactly one producer endpoint and one consumer
// endpoint, ordered through the tail/head release-acquire protocol.
unsafe impl<T: Send, const N: usize> Sync for InboundBlock<T, N> {}
unsafe impl<T: Send, const N: usize> Send for InboundBlock<T, N> {}

/// Whether the consumer may be asleep and must be woken out of band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    NotNeeded,
    Needed,
}

/// The only handle that can push into ring (src, dst). Not `Clone`, not `Sync`.
pub struct Producer<T, const N: usize> {
    block: Arc<InboundBlock<T, N>>,
    src: usize,
    tail: u64,
    published: u64,
    cached_head: u64,
    _not_sync: PhantomData<core::cell::Cell<()>>,
}

impl<T, const N: usize> core::fmt::Debug for Producer<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Producer")
            .field("src", &self.src)
            .field("tail", &self.tail)
            .field("published", &self.published)
            .finish()
    }
}

impl<T: Send, const N: usize> Producer<T, N> {
    #[inline(always)]
    fn ring(&self) -> &Ring<T, N> {
        &self.block.rings[self.src]
    }

    /// Stage a message. Not visible to the consumer until `flush()`.
    #[inline]
    pub fn try_push(&mut self, v: T) -> Result<(), T> {
        if self.tail - self.cached_head == N as u64 {
            self.cached_head = self.ring().head.load(Acquire);
            if self.tail - self.cached_head == N as u64 {
                return Err(v);
            }
        }
        // SAFETY: slot `tail` is outside [head, tail), so the consumer does not read it,
        // and the Acquire load of `head` ordered the consumer's last read of it before us.
        unsafe { (*self.ring().slot(self.tail)).write(v) };
        self.tail += 1;
        Ok(())
    }

    /// Exact free capacity (refreshes the cached head). Only this producer can reduce it,
    /// which is what makes `admit_all_or_nothing` race-free.
    #[inline]
    pub fn free(&mut self) -> usize {
        self.cached_head = self.ring().head.load(Acquire);
        N - (self.tail - self.cached_head) as usize
    }

    #[inline]
    pub fn staged(&self) -> u64 {
        self.tail - self.published
    }

    /// Publish staged messages: one Release store + one doorbell RMW per batch.
    #[inline]
    pub fn flush(&mut self) -> Wake {
        if self.tail == self.published {
            return Wake::NotNeeded;
        }
        self.ring().tail.store(self.tail, Release);
        self.published = self.tail;
        let word = &self.block.doorbell[self.src / 64];
        word.fetch_or(1 << (self.src % 64), AcqRel);
        fence(SeqCst); // Dekker with Consumer::prepare_park
        if self.block.parked.load(Relaxed) != 0 {
            Wake::Needed
        } else {
            Wake::NotNeeded
        }
    }

    pub fn src(&self) -> usize {
        self.src
    }
}

impl<T, const N: usize> Drop for Producer<T, N> {
    fn drop(&mut self) {
        if self.tail != self.published {
            self.block.rings[self.src].tail.store(self.tail, Release);
            self.block.doorbell[self.src / 64].fetch_or(1 << (self.src % 64), AcqRel);
        }
    }
}

/// The consumer side of one core: all inbound lanes of `dst`.
pub struct Consumer<T, const N: usize> {
    block: Arc<InboundBlock<T, N>>,
    heads: Box<[u64]>,
    cached_tails: Box<[u64]>,
    /// Lanes known to have pending work (doorbell bits already consumed).
    pending: Box<[u64]>,
    _not_sync: PhantomData<core::cell::Cell<()>>,
}

impl<T, const N: usize> core::fmt::Debug for Consumer<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Consumer")
            .field("lanes", &self.heads.len())
            .finish()
    }
}

impl<T: Send, const N: usize> Consumer<T, N> {
    /// Drain up to `per_lane` messages from each signalled lane, in lane order.
    /// `f(src, msg)`. Returns the number of messages delivered.
    pub fn poll(&mut self, per_lane: usize, mut f: impl FnMut(usize, T)) -> usize {
        let mut delivered = 0;
        for w in 0..self.pending.len() {
            let mut bits = self.pending[w] | self.block.doorbell[w].swap(0, Acquire);
            let mut still = 0u64;
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let src = w * 64 + b;
                let (n, more) = self.drain_lane(src, per_lane, &mut f);
                delivered += n;
                if more {
                    still |= 1 << b;
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
            // SAFETY: h < published tail (Acquire), slot initialized and owned by us.
            let v = unsafe { (*ring.slot(h)).assume_init_read() };
            h += 1;
            f(src, v);
        }
        if n > 0 {
            ring.head.store(h, Release);
        }
        self.heads[src] = h;
        // A new doorbell may have been consumed while this lane still had an old
        // cached tail. Refresh on exhaustion before dropping its pending bit;
        // otherwise the final newly published batch can be stranded forever.
        if h == t {
            self.cached_tails[src] = ring.tail.load(Acquire);
        }
        (n, h != self.cached_tails[src])
    }

    /// Returns `true` if the caller may sleep (and must then wait for a wake CQE).
    /// Returns `false` (and clears `parked`) if work arrived in the meantime.
    pub fn prepare_park(&self) -> bool {
        if self.pending.iter().any(|&p| p != 0) {
            return false;
        }
        self.block.parked.store(1, Relaxed);
        fence(SeqCst); // Dekker with Producer::flush
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

/// `producers[src][dst]`, `consumers[dst]`.
#[derive(Debug)]
pub struct Mesh<T, const N: usize> {
    pub producers: Vec<Vec<Producer<T, N>>>,
    pub consumers: Vec<Consumer<T, N>>,
}

impl<T: Send, const N: usize> Mesh<T, N> {
    /// Build the full grid. In production call `InboundBlock` construction on each
    /// consumer's pinned thread (first-touch NUMA placement); here all are built inline.
    pub fn new(producers: usize, consumers: usize) -> Self {
        const { assert!(N.is_power_of_two() && N >= 2) };
        assert!(producers >= 1 && consumers >= 1);
        let words = producers.div_ceil(64);
        let mut prod: Vec<Vec<Producer<T, N>>> = (0..producers)
            .map(|_| Vec::with_capacity(consumers))
            .collect();
        let mut cons = Vec::with_capacity(consumers);
        for _dst in 0..consumers {
            let block = Arc::new(InboundBlock {
                doorbell: (0..words).map(|_| CachePadded(AtomicU64::new(0))).collect(),
                parked: CachePadded(AtomicU32::new(0)),
                rings: (0..producers).map(|_| Ring::new()).collect(),
            });
            for (src, row) in prod.iter_mut().enumerate() {
                row.push(Producer {
                    block: block.clone(),
                    src,
                    tail: 0,
                    published: 0,
                    cached_head: 0,
                    _not_sync: PhantomData,
                });
            }
            cons.push(Consumer {
                block,
                heads: vec![0; producers].into(),
                cached_tails: vec![0; producers].into(),
                pending: vec![0; words].into(),
                _not_sync: PhantomData,
            });
        }
        Mesh {
            producers: prod,
            consumers: cons,
        }
    }
}

/// Sequencer side: deliver one multi-core transaction to *every* participant lane or to
/// none. Correct only because the sequencer is the single producer on its own lanes:
/// capacity observed by `free()` can only grow before the pushes.
pub fn admit_all_or_nothing<T: Send + Clone, const N: usize>(
    row: &mut [Producer<T, N>],
    participant_cores: &[usize],
    msg: impl Fn(usize, usize) -> T,
) -> Result<(), ()> {
    if participant_cores.iter().any(|&c| row[c].free() == 0) {
        return Err(());
    }
    for (i, &c) in participant_cores.iter().enumerate() {
        if row[c].try_push(msg(i, c)).is_err() {
            unreachable!("capacity reserved by the single producer");
        }
    }
    Ok(())
}

/// Separate fixed-capacity control lanes ensure user-data saturation cannot consume
/// vote/decision/credit capacity. The reactor must always drain control before data.
pub struct PriorityMesh<T: Send, C: Send, const DATA: usize, const CONTROL: usize> {
    pub data: Mesh<T, DATA>,
    pub control: Mesh<C, CONTROL>,
}
impl<T: Send, C: Send, const D: usize, const C0: usize> PriorityMesh<T, C, D, C0> {
    pub fn new(cores: usize) -> Self {
        Self {
            data: Mesh::new(cores, cores),
            control: Mesh::new(cores, cores),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_delivers_fifo_per_lane_without_loss_under_concurrency() {
        const C: usize = 4;
        const P: usize = 5; // 4 reactors + sequencer
        const PER: u64 = if cfg!(miri) { 64 } else { 50_000 };
        let Mesh {
            producers,
            consumers,
        } = Mesh::<u64, 256>::new(P, C);
        let mut handles = Vec::new();
        for (src, mut row) in producers.into_iter().enumerate() {
            handles.push(std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                for i in 0..PER {
                    for (dst, p) in row.iter_mut().enumerate() {
                        let v = (src as u64) << 48 | (dst as u64) << 40 | i;
                        let mut v2 = v;
                        loop {
                            match p.try_push(v2) {
                                Ok(()) => break,
                                Err(back) => {
                                    v2 = back;
                                    assert!(
                                        cfg!(miri) || std::time::Instant::now() < deadline,
                                        "producer progress deadline"
                                    );
                                    p.flush();
                                    std::thread::yield_now(); // 9 threads may share 2 CPUs
                                }
                            }
                        }
                        if i % 32 == 0 {
                            p.flush();
                        }
                    }
                }
                for p in row.iter_mut() {
                    p.flush();
                }
            }));
        }
        let mut cons_handles = Vec::new();
        for (dst, mut c) in consumers.into_iter().enumerate() {
            cons_handles.push(std::thread::spawn(move || {
                let mut next = vec![0u64; P];
                let mut total = 0u64;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while total < PER * P as u64 {
                    let n = c.poll(64, |src, v| {
                        assert_eq!((v >> 48) as usize, src);
                        assert_eq!((v >> 40 & 0xFF) as usize, dst, "misrouted");
                        assert_eq!(v & ((1 << 40) - 1), next[src], "lost/duplicated/reordered");
                        next[src] += 1;
                    }) as u64;
                    if n == 0 {
                        assert!(
                            cfg!(miri) || std::time::Instant::now() < deadline,
                            "consumer progress deadline"
                        );
                        std::thread::yield_now();
                    }
                    total += n;
                }
                total
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        for h in cons_handles {
            assert_eq!(h.join().unwrap(), PER * P as u64);
        }
    }

    #[test]
    fn a_new_doorbell_cannot_strand_data_behind_the_cached_tail() {
        let Mesh {
            mut producers,
            mut consumers,
        } = Mesh::<u64, 8>::new(1, 1);
        producers[0][0].try_push(10).unwrap();
        producers[0][0].try_push(11).unwrap();
        producers[0][0].flush();
        let mut seen = Vec::new();
        assert_eq!(consumers[0].poll(1, |_, v| seen.push(v)), 1);
        producers[0][0].try_push(12).unwrap();
        producers[0][0].flush();
        // No producer publishes anything after this point. The consumer must retain
        // pending work even when it consumes the last doorbell before the old tail.
        consumers[0].poll(8, |_, v| seen.push(v));
        consumers[0].poll(8, |_, v| seen.push(v));
        assert_eq!(seen, vec![10, 11, 12]);
    }

    #[test]
    fn all_or_nothing_admission_and_park_protocol() {
        let Mesh {
            mut producers,
            mut consumers,
        } = Mesh::<MeshMsg, 4>::new(3, 2);
        let seq_row = &mut producers[2];
        // Fill core 1's sequencer lane.
        for _ in 0..4 {
            seq_row[1].try_push(MeshMsg::default()).unwrap();
        }
        let mk = |p: usize, _c: usize| MeshMsg {
            participant: p as u8,
            participants: 2,
            ..Default::default()
        };
        assert!(admit_all_or_nothing(seq_row, &[0, 1], mk).is_err());
        assert_eq!(seq_row[0].staged(), 0, "nothing delivered to core 0 either");
        seq_row[1].flush();
        assert_eq!(consumers[1].poll(16, |_, _| {}), 4);
        assert!(admit_all_or_nothing(seq_row, &[0, 1], mk).is_ok());

        // Parking: consumer 0 parks, producer flush reports Wake::Needed.
        assert!(consumers[0].prepare_park());
        assert_eq!(producers[2][0].flush(), Wake::Needed);
        consumers[0].unpark();
        assert!(
            !consumers[0].prepare_park(),
            "doorbell pending: must not sleep"
        );
        let mut got = Vec::new();
        consumers[0].poll(16, |src, m| got.push((src, m.participant)));
        assert_eq!(got, vec![(2, 0)]);
    }
}
