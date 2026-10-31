//! io_uring backend for `IoBackend`.
//!
//! * One ring per reactor core: `SINGLE_ISSUER | DEFER_TASKRUN | COOP_TASKRUN`, so no
//!   SQ locking and no task-work IPIs; completions run only when the owner reaps.
//! * Registered files (`types::Fixed`) and registered buffers (`WriteFixed`/`ReadFixed`)
//!   remove per-op fget/fput and page pinning. Buffers are registered in <= 1 GiB
//!   chunks (kernel limit per iovec); unregistered addresses fall back to `Write`/`Read`.
//! * Durability per op via `RWF_DSYNC` (FUA on NVMe when the device has a volatile
//!   write cache, plain write otherwise) - no separate fsync round trip.
//! * Cross-core wakeups via `IORING_OP_MSG_RING`, which posts a CQE directly into the
//!   target core's ring: a parked reactor blocked in `submit_and_wait` wakes with no
//!   eventfd and no syscall on its side.

use crate::io::{IoBackend, Op, SqFull, UserData};
use io_uring::{IoUring, opcode, squeue, types};
use std::io;
use std::os::fd::{AsRawFd, RawFd};

const MAX_REG_CHUNK: usize = 1 << 30;

#[derive(Debug, Clone, Copy)]
struct RegBuf {
    addr: usize,
    len: usize,
    index: u16,
}

pub struct UringIo {
    ring: IoUring,
    bufs: Vec<RegBuf>,
    files_registered: usize,
}

impl core::fmt::Debug for UringIo {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UringIo")
            .field("fd", &self.ring.as_raw_fd())
            .field("bufs", &self.bufs.len())
            .finish()
    }
}

impl UringIo {
    /// `entries` SQEs (CQ = 2x). Must be created on the thread that will submit.
    pub fn new(entries: u32) -> io::Result<UringIo> {
        let ring = IoUring::builder()
            .setup_single_issuer()
            .setup_defer_taskrun()
            .setup_coop_taskrun()
            .setup_cqsize(entries * 2)
            .build(entries)
            .or_else(|_| IoUring::builder().setup_cqsize(entries * 2).build(entries))?; // kernels < 6.1
        Ok(UringIo {
            ring,
            bufs: Vec::new(),
            files_registered: 0,
        })
    }

    /// Register the log/extent files; `file` arguments are indices into `fds`.
    pub fn register_files(&mut self, fds: &[RawFd]) -> io::Result<()> {
        self.ring.submitter().register_files(fds)?;
        self.files_registered = fds.len();
        Ok(())
    }

    /// Register a memory region (slot arena, log buffers) as fixed buffers.
    ///
    /// # Safety
    /// `ptr..ptr+len` must remain mapped until the ring is dropped.
    pub unsafe fn register_region(&mut self, ptr: *mut u8, len: usize) -> io::Result<()> {
        let mut iov = Vec::new();
        let mut off = 0;
        while off < len {
            let n = (len - off).min(MAX_REG_CHUNK);
            iov.push(libc::iovec {
                iov_base: unsafe { ptr.add(off) }.cast(),
                iov_len: n,
            });
            off += n;
        }
        // Registration is all-at-once per ring; re-register the union.
        let mut all: Vec<libc::iovec> = self
            .bufs
            .iter()
            .map(|b| libc::iovec {
                iov_base: b.addr as *mut libc::c_void,
                iov_len: b.len,
            })
            .collect();
        all.extend_from_slice(&iov);
        if all.len() > u16::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many fixed buffers",
            ));
        }
        if !self.bufs.is_empty() {
            let _ = self.ring.submitter().unregister_buffers();
        }
        // SAFETY: caller guarantees lifetime of the new region; old regions were
        // guaranteed by earlier calls.
        unsafe { self.ring.submitter().register_buffers(&all)? };
        self.bufs = all
            .iter()
            .enumerate()
            .map(|(i, v)| RegBuf {
                addr: v.iov_base as usize,
                len: v.iov_len,
                index: i as u16,
            })
            .collect();
        Ok(())
    }

    fn buf_index(&self, addr: usize, len: usize) -> Option<u16> {
        self.bufs
            .iter()
            .find(|b| addr >= b.addr && addr + len <= b.addr + b.len)
            .map(|b| b.index)
    }

    fn push(&mut self, e: squeue::Entry) -> Result<(), SqFull> {
        // SAFETY: buffer validity is the caller's obligation (IoBackend contract).
        unsafe { self.ring.submission().push(&e).map_err(|_| SqFull) }
    }

    /// Flush queued SQEs to the kernel without waiting.
    pub fn submit(&mut self) -> io::Result<usize> {
        self.ring.submit()
    }

    /// Submit and block until at least `want` CQEs are available (parked reactor).
    pub fn submit_and_wait(&mut self, want: usize) -> io::Result<usize> {
        self.ring.submit_and_wait(want)
    }

    /// Drain all available CQEs: `f(user_data, res)`. Returns the count.
    pub fn reap(&mut self, mut f: impl FnMut(u64, i32)) -> usize {
        let mut n = 0;
        for cqe in self.ring.completion() {
            f(cqe.user_data(), cqe.result());
            n += 1;
        }
        n
    }

    /// Post a wake CQE into another core's ring (doorbell wakeup). The posted
    /// `user_data` always decodes as `Op::Wake` (R10-4): safe code cannot inject a
    /// LogWrite/Checkpoint/Load token that a reactor would route to an `on_cqe` handler.
    pub fn post_wake(&mut self, target_ring_fd: RawFd, wake: u32) -> Result<(), SqFull> {
        let user_data = UserData {
            op: Op::Wake,
            object: 0,
            ticket: wake,
        }
        .encode();
        let e = opcode::MsgRingData::new(types::Fd(target_ring_fd), 0, user_data, None)
            .build()
            .flags(squeue::Flags::SKIP_SUCCESS)
            .user_data(0);
        self.push(e)
    }

    pub fn ring_fd(&self) -> RawFd {
        self.ring.as_raw_fd()
    }
}

impl IoBackend for UringIo {
    unsafe fn write_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *const u8,
        len: u32,
        offset: u64,
        dsync: bool,
    ) -> Result<(), SqFull> {
        debug_assert!((file as usize) < self.files_registered);
        let flags = if dsync { libc::RWF_DSYNC } else { 0 };
        let fd = types::Fixed(file as u32);
        let e = match self.buf_index(buf as usize, len as usize) {
            Some(idx) => opcode::WriteFixed::new(fd, buf, len, idx)
                .offset(offset)
                .rw_flags(flags)
                .build(),
            None => opcode::Write::new(fd, buf, len)
                .offset(offset)
                .rw_flags(flags)
                .build(),
        };
        self.push(e.user_data(user_data))
    }

    unsafe fn read_at(
        &mut self,
        user_data: u64,
        file: u16,
        buf: *mut u8,
        len: u32,
        offset: u64,
    ) -> Result<(), SqFull> {
        debug_assert!((file as usize) < self.files_registered);
        let fd = types::Fixed(file as u32);
        let e = match self.buf_index(buf as usize, len as usize) {
            Some(idx) => opcode::ReadFixed::new(fd, buf, len, idx)
                .offset(offset)
                .build(),
            None => opcode::Read::new(fd, buf, len).offset(offset).build(),
        };
        self.push(e.user_data(user_data))
    }
}
