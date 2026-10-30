//! Preallocated intrusive FIFO, ordered by monotonically assigned first-dirty LSN.
use crate::PinAction;
use std::time::Duration;
const NIL: u32 = u32::MAX;
#[derive(Debug)]
pub struct TruncationPins {
    prev: Box<[u32]>,
    next: Box<[u32]>,
    lsn: Box<[u64]>,
    since: Box<[u64]>,
    latest: Box<[u64]>,
    head: u32,
    tail: u32,
    last_lsn: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum DirtyError {
    InvalidTenant,
    LsnRegression,
    ZeroLsn,
    StaleCheckpoint,
}
impl TruncationPins {
    pub fn new(tenants: usize) -> Self {
        assert!(tenants < NIL as usize);
        Self {
            prev: vec![NIL; tenants].into(),
            next: vec![NIL; tenants].into(),
            lsn: vec![0; tenants].into(),
            since: vec![0; tenants].into(),
            latest: vec![0; tenants].into(),
            head: NIL,
            tail: NIL,
            last_lsn: 0,
        }
    }
    pub fn on_dirty(&mut self, t: u32, lsn: u64, now: u64) -> Result<(), DirtyError> {
        let i = t as usize;
        if i >= self.lsn.len() {
            return Err(DirtyError::InvalidTenant);
        }
        if lsn == 0 {
            return Err(DirtyError::ZeroLsn);
        }
        if lsn < self.last_lsn {
            return Err(DirtyError::LsnRegression);
        }
        self.last_lsn = lsn;
        self.latest[i] = lsn;
        if self.lsn[i] != 0 {
            return Ok(());
        }
        self.lsn[i] = lsn;
        self.since[i] = now;
        self.prev[i] = self.tail;
        self.next[i] = NIL;
        if self.tail == NIL {
            self.head = t
        } else {
            self.next[self.tail as usize] = t
        }
        self.tail = t;
        Ok(())
    }
    pub fn on_checkpointed(&mut self, t: u32, image_lsn: u64) -> Result<(), DirtyError> {
        let i = t as usize;
        if i >= self.lsn.len() {
            return Err(DirtyError::InvalidTenant);
        }
        if self.latest[i] > image_lsn {
            return Err(DirtyError::StaleCheckpoint);
        }
        if self.lsn[i] == 0 {
            return Ok(());
        }
        let (p, n) = (self.prev[i], self.next[i]);
        if p == NIL {
            self.head = n
        } else {
            self.next[p as usize] = n
        }
        if n == NIL {
            self.tail = p
        } else {
            self.prev[n as usize] = p
        }
        self.lsn[i] = 0;
        self.prev[i] = NIL;
        self.next[i] = NIL;
        Ok(())
    }
    pub fn min_needed_lsn(&self, current: u64) -> u64 {
        if self.head == NIL {
            current
        } else {
            self.lsn[self.head as usize]
        }
    }
    pub fn pin_holder(&self, now: u64) -> Option<(u32, u64, Duration)> {
        if self.head == NIL {
            None
        } else {
            let i = self.head as usize;
            Some((
                self.head,
                self.lsn[i],
                Duration::from_nanos(now.saturating_sub(self.since[i])),
            ))
        }
    }
    pub fn decide(
        &self,
        now: u64,
        free: f64,
        max_age: Duration,
        degraded: impl Fn(u32) -> bool,
    ) -> PinAction {
        let Some((t, _, age)) = self.pin_holder(now) else {
            return PinAction::None;
        };
        if degraded(t) {
            PinAction::Relocate { tenant: t }
        } else if !free.is_finite() || free < 0.25 || age > max_age {
            PinAction::FreezeCheckpoint { tenant: t }
        } else {
            PinAction::None
        }
    }
}
