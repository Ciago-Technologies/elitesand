//! I/O backend abstraction shared by the slot lifecycle and the group-commit log, plus a
//! deterministic simulator used to test every CQE interleaving (reordering, errors,
//! short writes, torn writes, power loss).

/// Submission queue is full; nothing was queued, no CQE will arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqFull;

/// Operation class, top byte of `user_data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    LogWrite = 1,
    Checkpoint = 2,
    Load = 3,
    Wake = 4,
    Superblock = 5,
}

impl Op {
    pub fn from_u8(v: u8) -> Option<Op> {
        Some(match v {
            1 => Op::LogWrite,
            2 => Op::Checkpoint,
            3 => Op::Load,
            4 => Op::Wake,
            5 => Op::Superblock,
            _ => return None,
        })
    }
}

/// `user_data` = `[ op:8 | object:24 | ticket:32 ]`.
/// `object` is a slot index (within the core's range) or a log buffer index; `ticket`
/// is that object's per-submission counter, so a late CQE for an earlier incarnation of
/// the same object is recognized and ignored instead of corrupting the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserData {
    pub op: Op,
    pub object: u32,
    pub ticket: u32,
}

impl UserData {
    #[inline(always)]
    pub const fn encode(self) -> u64 {
        assert!(self.object < (1 << 24));
        (self.op as u64) << 56 | (self.object as u64) << 32 | self.ticket as u64
    }
    #[inline(always)]
    pub fn decode(raw: u64) -> Option<UserData> {
        Some(UserData {
            op: Op::from_u8((raw >> 56) as u8)?,
            object: ((raw >> 32) & 0xFF_FFFF) as u32,
            ticket: raw as u32,
        })
    }
}

/// Contract of a submission: `Ok(())` means exactly one CQE carrying `user_data` *will*
/// be delivered later (success, error, or `-ECANCELED`). Until it is, the buffer belongs
/// to the kernel/device.
pub trait IoBackend {
    /// # Safety
    /// `buf..buf+len` must stay allocated and must not be written by the CPU until the
    /// CQE for `user_data` has been delivered. `buf`, `len`, `offset` must satisfy the
    /// file's O_DIRECT alignment (logical block size).
    unsafe fn write_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *const u8,
        len: u32,
        offset: u64,
        dsync: bool,
    ) -> Result<(), SqFull>;

    /// # Safety
    /// `buf..buf+len` must stay allocated and must not be read as initialized data or
    /// written by the CPU until the CQE for `user_data` has been delivered.
    unsafe fn read_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *mut u8,
        len: u32,
        offset: u64,
    ) -> Result<(), SqFull>;
}

// ---------------------------------------------------------------------------------
// Deterministic simulator
// ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimKind {
    Write { dsync: bool },
    Read,
}

#[derive(Debug, Clone, Copy)]
pub struct SimOp {
    pub user_data: u64,
    pub file: u16,
    pub kind: SimKind,
    pub buf: *mut u8,
    pub len: u32,
    pub offset: u64,
}

/// How the simulated device completes one queued op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    /// CQE with `-errno`; no bytes transferred.
    Err(i32),
    /// Transfer only the first `n` bytes and report `n` (short write/read).
    Short(u32),
}

/// In-memory device: a byte vector per file, a queue of in-flight ops, an optional SQ
/// capacity. Data moves at *completion* time, which models DMA happening any time
/// between submission and CQE: if the engine touched a buffer before its CQE, the
/// simulator persists the corrupted bytes and the tests catch it.
#[derive(Debug)]
pub struct SimIo {
    pub files: Vec<Vec<u8>>,
    pub inflight: Vec<SimOp>,
    pub sq_capacity: usize,
    pub submitted: u64,
    pub dsync_writes: u64,
}

impl SimIo {
    pub fn new(files: usize, bytes: usize) -> Self {
        SimIo {
            files: (0..files).map(|_| vec![0u8; bytes]).collect(),
            inflight: Vec::new(),
            sq_capacity: usize::MAX,
            submitted: 0,
            dsync_writes: 0,
        }
    }

    /// Complete in-flight op `i` (in any order) and return the CQE `(user_data, res)`.
    pub fn complete(&mut self, i: usize, outcome: Outcome) -> (u64, i32) {
        let op = self.inflight.swap_remove(i);
        let n = match outcome {
            Outcome::Ok => op.len,
            Outcome::Short(n) => n.min(op.len),
            Outcome::Err(e) => return (op.user_data, -e),
        } as usize;
        let file = &mut self.files[op.file as usize];
        let off = op.offset as usize;
        // SAFETY: the engine promised the buffer is live until this CQE.
        unsafe {
            match op.kind {
                SimKind::Write { .. } => {
                    file[off..off + n].copy_from_slice(core::slice::from_raw_parts(op.buf, n))
                }
                SimKind::Read => {
                    core::ptr::copy_nonoverlapping(file[off..off + n].as_ptr(), op.buf, n)
                }
            }
        }
        (op.user_data, n as i32)
    }

    /// Simulated power loss: in-flight writes either never happened or are torn at a
    /// 4 KiB boundary chosen by `torn_at(op_index) -> bytes_persisted`.
    pub fn power_loss(&mut self, mut torn_at: impl FnMut(usize, &SimOp) -> u32) {
        let ops = core::mem::take(&mut self.inflight);
        for (i, op) in ops.iter().enumerate() {
            if let SimKind::Write { .. } = op.kind {
                let n = (torn_at(i, op).min(op.len) / 4096 * 4096) as usize;
                let off = op.offset as usize;
                // SAFETY: as in `complete`.
                unsafe {
                    self.files[op.file as usize][off..off + n]
                        .copy_from_slice(core::slice::from_raw_parts(op.buf, n))
                };
            }
        }
    }

    pub fn find(&self, pred: impl Fn(&SimOp) -> bool) -> Option<usize> {
        self.inflight.iter().position(pred)
    }
}

impl IoBackend for SimIo {
    unsafe fn write_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *const u8,
        len: u32,
        offset: u64,
        dsync: bool,
    ) -> Result<(), SqFull> {
        if self.inflight.len() >= self.sq_capacity {
            return Err(SqFull);
        }
        assert!(
            offset % 4096 == 0 && len % 4096 == 0 && (buf as usize) % 4096 == 0,
            "O_DIRECT alignment violated"
        );
        self.submitted += 1;
        self.dsync_writes += dsync as u64;
        self.inflight.push(SimOp {
            user_data,
            file,
            kind: SimKind::Write { dsync },
            buf: buf as *mut u8,
            len,
            offset,
        });
        Ok(())
    }
    unsafe fn read_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *mut u8,
        len: u32,
        offset: u64,
    ) -> Result<(), SqFull> {
        if self.inflight.len() >= self.sq_capacity {
            return Err(SqFull);
        }
        assert!(
            offset % 4096 == 0 && len % 4096 == 0 && (buf as usize) % 4096 == 0,
            "O_DIRECT alignment violated"
        );
        self.submitted += 1;
        self.inflight.push(SimOp {
            user_data,
            file,
            kind: SimKind::Read,
            buf,
            len,
            offset,
        });
        Ok(())
    }
}

/// Small deterministic PRNG for tests (xorshift64*).
#[derive(Debug, Clone)]
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}
