//! Budgeted private engine writer plus a sealed agent descriptor. No raw owned fd escapes.
use std::{
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
    sync::{Arc, Mutex},
};
pub struct ExportBudget {
    max_bytes: usize,
    max_sessions: usize,
    used: Mutex<(usize, usize)>,
}
impl ExportBudget {
    pub fn new(max_bytes: usize, max_sessions: usize) -> Arc<Self> {
        Arc::new(Self {
            max_bytes,
            max_sessions,
            used: Mutex::new((0, 0)),
        })
    }
    pub fn usage(&self) -> (usize, usize) {
        *self.used.lock().unwrap()
    }
    fn reserve(self: &Arc<Self>, len: usize) -> Result<Reservation, String> {
        let mut used = self.used.lock().map_err(|_| "budget lock poisoned")?;
        if used.1 >= self.max_sessions || len > self.max_bytes.saturating_sub(used.0) {
            return Err("export budget exhausted".into());
        }
        used.0 += len;
        used.1 += 1;
        Ok(Reservation {
            budget: self.clone(),
            len,
        })
    }
}
struct Reservation {
    budget: Arc<ExportBudget>,
    len: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut used) = self.budget.used.lock() {
            used.0 -= self.len;
            used.1 -= 1;
        }
    }
}
struct Mapping {
    ptr: NonNull<libc::c_void>,
    len: usize,
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr.as_ptr(), self.len);
        }
    }
}
fn mapping(fd: BorrowedFd<'_>, len: usize, prot: i32) -> Result<Mapping, String> {
    // SAFETY: checked length and borrowed live descriptor; Mapping unmaps exactly once.
    let p = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            len,
            prot,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(Mapping {
        ptr: NonNull::new(p).ok_or("null mapping")?,
        len,
    })
}
/// Probe write sealing on an O_RDWR descriptor. Testing only O_RDONLY would be vacuous.
pub fn verify_write_seal(fd: BorrowedFd<'_>, len: usize) -> Result<(), String> {
    let wanted =
        libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL;
    let seals = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS) };
    if seals < 0 || seals & wanted != wanted {
        return Err("required memfd seals missing".into());
    }
    let read = mapping(fd, len, libc::PROT_READ)?;
    let upgraded =
        unsafe { libc::mprotect(read.ptr.as_ptr(), len, libc::PROT_READ | libc::PROT_WRITE) };
    if upgraded == 0 {
        return Err("kernel allowed writable upgrade of a write-sealed O_RDWR mapping".into());
    }
    if let Ok(writable) = mapping(fd, len, libc::PROT_READ | libc::PROT_WRITE) {
        drop(writable);
        return Err("kernel allowed new writable sealed mapping".into());
    }
    Ok(())
}
pub struct SealedExportWindow {
    _engine: OwnedFd,
    agent: OwnedFd,
    _mapping: Mapping,
    _reservation: Reservation,
}
impl SealedExportWindow {
    pub fn create_and_selftest(len: usize, budget: &Arc<ExportBudget>) -> Result<Self, String> {
        if len < 4096 || len % 4096 != 0 || len > isize::MAX as usize {
            return Err("invalid export window size".into());
        }
        let reservation = budget.reserve(len)?;
        let raw = unsafe {
            libc::memfd_create(
                c"ciagomem-export".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let engine = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { libc::ftruncate(engine.as_raw_fd(), len as libc::off_t) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let map = mapping(engine.as_fd(), len, libc::PROT_READ | libc::PROT_WRITE)?;
        let seals =
            libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(engine.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // This descriptor is write-capable, matching the procfs O_RDWR reopen attack.
        verify_write_seal(engine.as_fd(), len)?;
        let path = std::ffi::CString::new(format!("/proc/self/fd/{}", engine.as_raw_fd())).unwrap();
        let ro = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if ro < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            _engine: engine,
            agent: unsafe { OwnedFd::from_raw_fd(ro) },
            _mapping: map,
            _reservation: reservation,
        })
    }
    pub fn agent_fd(&self) -> BorrowedFd<'_> {
        self.agent.as_fd()
    }
    pub fn len(&self) -> usize {
        self._mapping.len
    }
    pub fn is_empty(&self) -> bool {
        false
    }
}
