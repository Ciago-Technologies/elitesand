//! Sound sequence lock (Boehm, "Can Seqlocks Get Along With Programming Language
//! Memory Models?", MSPC'12, the "fence" variant).
//!
//! Why the spec's version is broken:
//!   * It reads the protected FlatBuffer bytes with plain (non-atomic) loads while the
//!     writer stores to them. That is a data race, i.e. undefined behaviour in Rust,
//!     regardless of the retry loop. The compiler may tear, cache or re-load values.
//!   * Its validation is `seq.load(Acquire)` *after* the data loads. An acquire load only
//!     orders *later* accesses; earlier data loads may sink below it. On ARM64 (and under
//!     the language model on x86) a reader can observe new data with the old sequence
//!     and accept a torn snapshot.
//!
//! The protocol here:
//!   * Every protected word is an `AtomicU64`; readers and writer use the same 8-byte
//!     access width (mixed-size racing atomics are UB too).
//!   * Reader: `s1 = seq.load(Relaxed); fence(Acquire); <relaxed data loads>; fence(Acquire);
//!     s2 = seq.load(Relaxed)`; accept iff `s1 == s2 && s1` even.
//!   * Writer: `CAS seq even->odd (Acquire); fence(Release); <relaxed data stores>;
//!     seq.store(odd+1, Release)`.
//!
//!   Correctness: if a reader's relaxed data load observes a store made after the writer's
//!   release fence, the writer's release fence synchronizes-with the reader's second
//!   acquire fence, so the odd sequence store happens-before the reader's `s2` load and
//!   `s2 != s1`. If `s1` reads the even value written by `write_end`, the release store
//!   synchronizes with the first acquire fence and all prior data stores are visible.
//!
//! Readers only ever issue `Relaxed` 8-byte loads plus fences. Per the Rust atomics
//! documentation that is the only form guaranteed sound on *read-only mappings*
//! (agent processes map the slab `PROT_READ`). `load(Acquire)` is not guaranteed there.

use core::sync::atomic::{
    AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
    fence,
};

/// Default bounded retry budget for optimistic readers. Readers never block the writer;
/// when the budget is exhausted the caller falls back to the owner core's mailbox.
pub const READ_RETRY_LIMIT: u32 = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Writer held the lock (or kept changing it) for the whole retry budget.
    /// Also returned forever for a poisoned (crashed mid-write) section.
    WriterActive,
    /// The closure rejected a snapshot that *did* validate: the layout itself is corrupt
    /// (e.g. a FlatBuffer offset outside the slot). Not retryable.
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterBusy;

/// The sequence word. Even = stable, odd = write in progress. 64 bits never wraps
/// in practice (2^63 writes at 1 GHz is 292 years).
#[derive(Debug, Default)]
#[repr(transparent)]
pub struct SeqWord(AtomicU64);

impl SeqWord {
    pub const fn new() -> Self {
        SeqWord(AtomicU64::new(0))
    }

    /// Start an optimistic read. `None` while a writer is active.
    #[inline(always)]
    pub fn read_begin(&self) -> Option<u64> {
        let s = self.0.load(Relaxed);
        fence(Acquire);
        if s & 1 == 0 { Some(s) } else { None }
    }

    /// `true` if the section must be retried. Call after the last data load.
    #[inline(always)]
    pub fn read_retry(&self, s1: u64) -> bool {
        fence(Acquire);
        self.0.load(Relaxed) != s1
    }

    /// Enter the exclusive write section. The CAS (instead of a plain store) turns a
    /// routing bug that lets two cores write one slot into a detectable error rather
    /// than a silently torn slot. Cost: one uncontended `lock cmpxchg` / `casa`.
    #[inline(always)]
    pub fn write_begin(&self) -> Result<SeqWriteGuard<'_>, WriterBusy> {
        let s = self.0.load(Relaxed);
        if s & 1 != 0 {
            return Err(WriterBusy);
        }
        if self.0.compare_exchange(s, s + 1, Acquire, Relaxed).is_err() {
            return Err(WriterBusy);
        }
        // Orders the odd store before every data store in the section.
        fence(Release);
        Ok(SeqWriteGuard {
            seq: self,
            odd: s + 1,
            committed: false,
        })
    }

    /// Raw value (diagnostics and tests).
    #[inline]
    pub fn raw(&self) -> u64 {
        self.0.load(Relaxed)
    }
}

/// Write-section guard. `commit()` publishes. Dropping without `commit()` leaves the
/// sequence odd ("poisoned"): readers get `WriterActive` instead of a half-applied
/// mutation. Under `panic = "abort"` this path is never reached; under unwinding it is
/// the only safe choice.
#[derive(Debug)]
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
        self.seq.0.store(self.odd + 1, Release);
    }
}

impl Drop for SeqWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Deliberately leave the sequence odd.
            debug_assert!(self.seq.raw() & 1 == 1);
        }
    }
}

/// Read-only, bounds-checked view of protected words, valid only inside a read section.
/// Every accessor is a relaxed 8-byte atomic load; nothing here can fault or panic on a
/// torn snapshot, it can only return garbage that validation then discards.
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
    pub fn len_words(&self) -> usize {
        self.words.len()
    }
    #[inline(always)]
    pub fn word(&self, i: usize) -> Option<u64> {
        self.words.get(i).map(|w| w.load(Relaxed))
    }
    /// Copy `dst.len()` words starting at `start`.
    #[inline]
    pub fn copy_words(&self, start: usize, dst: &mut [u64]) -> Option<()> {
        let src = self.words.get(start..start.checked_add(dst.len())?)?;
        for (d, s) in dst.iter_mut().zip(src) {
            *d = s.load(Relaxed);
        }
        Some(())
    }
    /// Copy arbitrary bytes (little-endian word image) without ever issuing a load
    /// narrower than 8 bytes, so it never mixes access sizes with the writer.
    #[inline]
    pub fn copy_bytes(&self, byte_off: usize, dst: &mut [u8]) -> Option<()> {
        let end = byte_off.checked_add(dst.len())?;
        if end > self.words.len() * 8 {
            return None;
        }
        let mut out = 0;
        let mut off = byte_off;
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
    #[inline]
    pub fn u16_at(&self, byte_off: usize) -> Option<u16> {
        let mut b = [0u8; 2];
        self.copy_bytes(byte_off, &mut b)?;
        Some(u16::from_le_bytes(b))
    }
    #[inline]
    pub fn u64_at(&self, byte_off: usize) -> Option<u64> {
        if byte_off % 8 == 0 {
            return self.word(byte_off / 8);
        }
        let mut b = [0u8; 8];
        self.copy_bytes(byte_off, &mut b)?;
        Some(u64::from_le_bytes(b))
    }
    #[inline]
    pub fn f64_at(&self, byte_off: usize) -> Option<f64> {
        self.u64_at(byte_off).map(f64::from_bits)
    }
}

/// Writer view. Only constructible from a live `SeqWriteGuard`'s section.
#[derive(Debug, Clone, Copy)]
pub struct WordsMut<'a> {
    words: &'a [AtomicU64],
}

impl<'a> WordsMut<'a> {
    /// # Safety contract (not `unsafe` because misuse cannot cause UB, only torn data):
    /// call only while holding the write section that protects `words`.
    #[inline(always)]
    pub(crate) fn new(words: &'a [AtomicU64]) -> Self {
        WordsMut { words }
    }
    #[inline(always)]
    pub fn set(&self, i: usize, v: u64) {
        self.words[i].store(v, Relaxed);
    }
    #[inline(always)]
    pub fn get(&self, i: usize) -> u64 {
        self.words[i].load(Relaxed)
    }
    #[inline(always)]
    pub fn len_words(&self) -> usize {
        self.words.len()
    }
    /// Byte write via 8-byte read-modify-write of the writer's own words. The writer is
    /// exclusive, so the RMW needs no atomic RMW instruction.
    pub fn write_bytes(&self, byte_off: usize, src: &[u8]) {
        assert!(
            byte_off + src.len() <= self.words.len() * 8,
            "write outside slot"
        );
        let mut done = 0;
        let mut off = byte_off;
        while done < src.len() {
            let idx = off / 8;
            let in_word = off % 8;
            let n = (8 - in_word).min(src.len() - done);
            let mut w = self.words[idx].load(Relaxed).to_le_bytes();
            w[in_word..in_word + n].copy_from_slice(&src[done..done + n]);
            self.words[idx].store(u64::from_le_bytes(w), Relaxed);
            done += n;
            off += n;
        }
    }
    pub fn set_u64_at(&self, byte_off: usize, v: u64) {
        if byte_off % 8 == 0 {
            self.set(byte_off / 8, v)
        } else {
            self.write_bytes(byte_off, &v.to_le_bytes())
        }
    }
    pub fn set_f64_at(&self, byte_off: usize, v: f64) {
        self.set_u64_at(byte_off, v.to_bits())
    }
    pub fn fill(&self, v: u64) {
        for w in self.words {
            w.store(v, Relaxed);
        }
    }
}

/// Generic optimistic read over `(seq, words)`.
///
/// The closure must be side-effect free: it may run several times and only the result
/// of a validated run is returned. Returning `None` means "this snapshot is not
/// interpretable"; if the snapshot then validates, the error is `Corrupt`.
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

/// Self-contained seqlock cell for small values (per-core stats, frame headers, ...).
#[derive(Debug)]
#[repr(C, align(64))]
pub struct SeqCell<const W: usize> {
    seq: SeqWord,
    words: [AtomicU64; W],
}

impl<const W: usize> Default for SeqCell<W> {
    fn default() -> Self {
        Self::new([0; W])
    }
}

impl<const W: usize> SeqCell<W> {
    pub(crate) fn new(init: [u64; W]) -> Self {
        SeqCell {
            seq: SeqWord::new(),
            words: init.map(AtomicU64::new),
        }
    }

    pub fn load(&self) -> Result<[u64; W], ReadError> {
        read_with(&self.seq, &self.words, READ_RETRY_LIMIT, |v| {
            let mut out = [0u64; W];
            v.copy_words(0, &mut out)?;
            Some(out)
        })
    }

    pub fn read<R>(&self, f: impl FnMut(WordView<'_>) -> Option<R>) -> Result<R, ReadError> {
        read_with(&self.seq, &self.words, READ_RETRY_LIMIT, f)
    }

    pub fn store(&self, v: [u64; W]) -> Result<(), WriterBusy> {
        self.write(|w| {
            for (i, x) in v.iter().enumerate() {
                w.set(i, *x);
            }
        })
    }

    pub fn write<R>(&self, f: impl FnOnce(WordsMut<'_>) -> R) -> Result<R, WriterBusy> {
        let g = self.seq.write_begin()?;
        let r = f(WordsMut::new(&self.words));
        g.commit();
        Ok(r)
    }

    pub fn seq(&self) -> &SeqWord {
        &self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    /// Invariant: every word equals word[0] and word[W-1] == !word[0]. A torn read
    /// breaks it. Run with `--release` on ARM64 to exercise weak ordering.
    #[test]
    fn never_returns_torn_snapshot() {
        const W: usize = 33;
        let cell = Arc::new(SeqCell::<W>::default());
        cell.store({
            let mut a = [0u64; W];
            a[W - 1] = !0;
            a
        })
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(4));
        let writer_start = start.clone();
        let w = {
            let (cell, stop) = (cell.clone(), stop.clone());
            std::thread::spawn(move || {
                writer_start.wait();
                let mut i = 0u64;
                let limit = if cfg!(miri) { 300 } else { 10_000 };
                while i < limit {
                    i += 1;
                    cell.write(|w| {
                        for k in 0..W - 1 {
                            w.set(k, i);
                        }
                        w.set(W - 1, !i);
                    })
                    .unwrap();
                    if i % 16 == 0 {
                        std::thread::yield_now();
                    }
                }
                stop.store(true, Relaxed);
                i
            })
        };
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let (cell, stop, start) = (cell.clone(), stop.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let mut ok = 0u64;
                    let minimum = if cfg!(miri) { 1 } else { 1000 };
                    while !stop.load(Relaxed) || ok < minimum {
                        if let Ok(a) = cell.load() {
                            let x = a[0];
                            assert!(a[..W - 1].iter().all(|&v| v == x), "torn snapshot");
                            assert_eq!(a[W - 1], !x, "torn snapshot");
                            ok += 1;
                        }
                    }
                    ok
                })
            })
            .collect();
        let writes = w.join().unwrap();
        let reads: u64 = readers.into_iter().map(|r| r.join().unwrap()).sum();
        assert_eq!(writes, if cfg!(miri) { 300 } else { 10_000 });
        assert!(reads >= if cfg!(miri) { 3 } else { 3000 }, "reads={reads}");
    }

    #[test]
    fn second_writer_is_rejected_and_poison_is_sticky() {
        let cell = SeqCell::<2>::default();
        let g = cell.seq().write_begin().unwrap();
        assert_eq!(cell.seq().write_begin().unwrap_err(), WriterBusy);
        drop(g); // not committed: poisoned
        assert_eq!(cell.load().unwrap_err(), ReadError::WriterActive);
    }

    #[test]
    fn byte_access_round_trips_across_word_boundaries() {
        let cell = SeqCell::<4>::default();
        cell.write(|w| w.write_bytes(5, &[1, 2, 3, 4, 5, 6, 7, 8, 9]))
            .unwrap();
        let got = cell
            .read(|v| {
                let mut b = [0u8; 9];
                v.copy_bytes(5, &mut b)?;
                Some(b)
            })
            .unwrap();
        assert_eq!(got, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            cell.read(|v| v.u32_at(6)).unwrap(),
            u32::from_le_bytes([2, 3, 4, 5])
        );
        assert_eq!(cell.read(|v| v.u64_at(30)).unwrap_err(), ReadError::Corrupt);
    }
}

use crate::{Clock, DeadlineReadError};
use std::time::Duration;
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
