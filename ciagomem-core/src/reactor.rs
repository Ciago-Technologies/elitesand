//! Bounded one-shot socket readiness. Native mode never silently falls back to poll(2).
use io_uring::{IoUring, opcode, types};
use std::{collections::BTreeMap, io, os::fd::RawFd};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Uring,
    Portable,
}
struct Entry {
    fd: RawFd,
    mask: u32,
    ticket: Option<u64>,
}
pub struct Readiness {
    ring: Option<IoUring>,
    entries: BTreeMap<u64, Entry>,
    pending: BTreeMap<u64, u64>,
    next: u64,
    limit: usize,
}
impl Readiness {
    pub fn new(mode: Mode, limit: usize) -> io::Result<Self> {
        if limit == 0 || limit > 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let ring = if mode == Mode::Uring {
            Some(
                IoUring::builder()
                    .setup_single_issuer()
                    .build((limit * 8).next_power_of_two() as u32)?,
            )
        } else {
            None
        };
        Ok(Self {
            ring,
            entries: BTreeMap::new(),
            pending: BTreeMap::new(),
            next: 1,
            limit,
        })
    }
    pub fn set(&mut self, owner: u64, fd: RawFd, write: bool) -> io::Result<()> {
        let mask = (libc::POLLIN | if write { libc::POLLOUT } else { 0 }) as u32;
        if self.entries.get(&owner).is_some_and(|e| {
            e.fd == fd && e.mask == mask && (self.ring.is_none() || e.ticket.is_some())
        }) {
            return Ok(());
        }
        self.remove(owner)?;
        if self.entries.len() >= self.limit || self.pending.len() >= self.limit * 4 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let ticket = if let Some(ring) = &mut self.ring {
            let ticket = self.next;
            self.next = self
                .next
                .checked_add(1)
                .filter(|n| *n < 1 << 63)
                .ok_or_else(|| io::Error::other("poll ticket exhausted"))?;
            let sqe = opcode::PollAdd::new(types::Fd(fd), mask)
                .build()
                .user_data(ticket);
            // SAFETY: PollAdd holds a kernel file reference and retains no user memory pointer.
            unsafe {
                ring.submission()
                    .push(&sqe)
                    .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
            }
            self.pending.insert(ticket, owner);
            Some(ticket)
        } else {
            None
        };
        self.entries.insert(owner, Entry { fd, mask, ticket });
        Ok(())
    }
    pub fn remove(&mut self, owner: u64) -> io::Result<()> {
        if let Some(old) = self.entries.remove(&owner) {
            if let (Some(ring), Some(ticket)) = (&mut self.ring, old.ticket) {
                let sqe = opcode::PollRemove::new(ticket)
                    .build()
                    .user_data(ticket | (1 << 63));
                // SAFETY: cancellation uses only a scalar poll ticket, never a payload pointer.
                unsafe {
                    ring.submission()
                        .push(&sqe)
                        .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
                }
            }
        }
        Ok(())
    }
    pub fn wait(&mut self, milliseconds: u32) -> io::Result<Vec<u64>> {
        if milliseconds > 1000 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if let Some(ring) = &mut self.ring {
            let time = types::Timespec::new()
                .sec((milliseconds / 1000) as u64)
                .nsec((milliseconds % 1000) * 1_000_000);
            let args = types::SubmitArgs::new().timespec(&time);
            match ring.submitter().submit_with_args(1, &args) {
                Ok(_) => {}
                Err(e) if matches!(e.raw_os_error(), Some(libc::ETIME | libc::EINTR)) => {}
                Err(e) => return Err(e),
            }
            let mut ready = Vec::new();
            for event in ring.completion() {
                let ticket = event.user_data();
                if ticket >> 63 != 0 {
                    continue;
                }
                if event.result() < 0 && event.result() != -libc::ECANCELED {
                    return Err(io::Error::from_raw_os_error(-event.result()));
                }
                if let Some(owner) = self.pending.remove(&ticket) {
                    if let Some(entry) = self.entries.get_mut(&owner) {
                        if entry.ticket == Some(ticket) {
                            entry.ticket = None;
                            if event.result() != -libc::ECANCELED {
                                ready.push(owner);
                            }
                        }
                    }
                }
            }
            Ok(ready)
        } else {
            let owners: Vec<_> = self.entries.keys().copied().collect();
            let mut fds: Vec<_> = self
                .entries
                .values()
                .map(|e| libc::pollfd {
                    fd: e.fd,
                    events: e.mask as i16,
                    revents: 0,
                })
                .collect();
            // SAFETY: fds is an initialized mutable array valid for this synchronous syscall.
            let n = unsafe {
                libc::poll(
                    fds.as_mut_ptr(),
                    fds.len() as libc::nfds_t,
                    milliseconds as i32,
                )
            };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    return Err(e);
                }
            }
            Ok(fds
                .iter()
                .zip(owners)
                .filter_map(|(fd, owner)| (fd.revents != 0).then_some(owner))
                .collect())
        }
    }
}
pub fn allowed_cpus() -> io::Result<Vec<usize>> {
    // SAFETY: cpu_set_t is a plain initialized bitset and is bounded by CPU_SETSIZE.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((0..libc::CPU_SETSIZE as usize)
            .filter(|n| libc::CPU_ISSET(*n, &set))
            .collect())
    }
}
pub fn pin(cpu: usize) -> io::Result<()> {
    if !allowed_cpus()?.contains(&cpu) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CPU outside allowed cpuset",
        ));
    }
    // SAFETY: validated CPU index and live bounded cpu_set_t storage.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of_val(&set), &set) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
extern "C" fn stop_signal(_: libc::c_int) {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::Relaxed);
}
/// Process-level helper for the standalone daemon; embedded callers own their signal policy.
pub fn install_shutdown_handlers() -> io::Result<()> {
    // SAFETY: the handler only stores a lock-free atomic; sigaction receives initialized storage.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop_signal as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGTERM, libc::SIGINT] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}
pub fn shutdown_requested() -> bool {
    SHUTDOWN.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, os::fd::AsRawFd, os::unix::net::UnixStream};
    fn progress(mode: Mode) -> io::Result<()> {
        let mut poll = Readiness::new(mode, 2)?;
        let (a, b) = UnixStream::pair()?;
        let mut writer = std::fs::File::from(std::os::fd::OwnedFd::from(b));
        poll.set(1, a.as_raw_fd(), false)?;
        assert!(poll.wait(1)?.is_empty());
        writer.write_all(b"ready")?;
        assert_eq!(poll.wait(50)?, vec![1]);
        poll.remove(1)?;
        assert!(poll.wait(1)?.is_empty());
        Ok(())
    }
    #[test]
    fn portable_readiness_tracks_removal() {
        progress(Mode::Portable).unwrap();
    }
    #[test]
    fn native_readiness_or_explicit_kernel_skip() {
        match progress(Mode::Uring) {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(libc::EPERM | libc::EACCES | libc::ENOSYS)
                ) =>
            {
                eprintln!("SKIP native socket readiness: {e}")
            }
            Err(e) => panic!("native readiness: {e}"),
        }
    }
}
