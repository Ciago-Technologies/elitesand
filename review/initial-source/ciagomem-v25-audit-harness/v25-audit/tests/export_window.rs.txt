//! Empirical check of the per-session export window recipe on the running kernel.
#![cfg(target_os = "linux")]
use std::ffi::CString;

unsafe fn errno() -> i32 { unsafe { *libc::__errno_location() } }

#[test]
fn sealed_readonly_export_window() {
    unsafe {
        let name = CString::new("ciago-export").unwrap();
        // Not MFD_HUGETLB: 4 KiB granularity, one window per session.
        let fd = libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING);
        assert!(fd >= 0);
        assert_eq!(libc::ftruncate(fd, 4096), 0);
        let rw = libc::mmap(core::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
        assert_ne!(rw, libc::MAP_FAILED);
        *(rw as *mut u64) = 0xC1A6;                       // engine copies tenant snapshot in
        // Seal: size fixed, no new writable mappings/writes, seals frozen.
        let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_FUTURE_WRITE | libc::F_SEAL_SEAL;
        assert_eq!(libc::fcntl(fd, libc::F_ADD_SEALS, seals), 0);
        // Engine's pre-existing RW mapping still works (engine keeps updating).
        *(rw as *mut u64) = 0xC1A7;
        // write(2) through any fd is now EPERM.
        assert_eq!(libc::write(fd, [1u8].as_ptr().cast(), 1), -1);
        assert_eq!(errno(), libc::EPERM);
        // Hand the agent an O_RDONLY re-open, never the original O_RDWR fd.
        let p = CString::new(format!("/proc/self/fd/{fd}")).unwrap();
        let ro = libc::open(p.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        assert!(ro >= 0);
        // Agent side: PROT_WRITE shared mapping refused.
        let w = libc::mmap(core::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, ro, 0);
        assert_eq!(w, libc::MAP_FAILED);
        // Agent side: read-only mapping works and sees engine updates.
        let r = libc::mmap(core::ptr::null_mut(), 4096, libc::PROT_READ, libc::MAP_SHARED, ro, 0);
        assert_ne!(r, libc::MAP_FAILED);
        assert_eq!(*(r as *const u64), 0xC1A7);
        // Agent side: mprotect() cannot upgrade the mapping.
        assert_eq!(libc::mprotect(r, 4096, libc::PROT_READ | libc::PROT_WRITE), -1);
        // Agent side: cannot shrink/punch to SIGBUS the engine.
        assert_eq!(libc::ftruncate(ro, 0), -1);
        assert_ne!(libc::fallocate(ro, libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE, 0, 4096), 0);
        // Seals are frozen.
        assert_eq!(libc::fcntl(fd, libc::F_ADD_SEALS, 0), -1);
        let got = libc::fcntl(ro, libc::F_GET_SEALS);
        assert_eq!(got & seals, seals);
        // CAUTION: the ORIGINAL O_RDWR fd still permits ftruncate? No: F_SEAL_SHRINK blocks it.
        assert_eq!(libc::ftruncate(fd, 0), -1);
        println!("export window: all seal checks passed on this kernel");
    }
}
