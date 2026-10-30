//! Linux filesystem adapter anchored to an owned directory descriptor, not string prefixes.
use crate::{OP_OPERATOR, OP_READ, VerifiedMacaroon};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
};
pub struct LocalExportRoot {
    directory: File,
    tenant: u128,
}
impl LocalExportRoot {
    /// Trusted operator configuration: one tenant's export directory, opened once.
    pub fn open(path: &Path, tenant: u128) -> std::io::Result<Self> {
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        Ok(Self { directory, tenant })
    }
    pub fn create(&self, session: &VerifiedMacaroon, relative: &str) -> std::io::Result<File> {
        if session.tenant_incarnation() != self.tenant
            || !session.allows(OP_OPERATOR | OP_READ)
            || relative.is_empty()
            || Path::new(relative).is_absolute()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "export authority denied",
            ));
        }
        let path = std::ffi::CString::new(relative).map_err(std::io::Error::other)?;
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        let how = OpenHow {
            flags: (libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC) as u64,
            mode: 0o600,
            resolve: 0x08 | 0x04 | 0x02,
        };
        // SAFETY: valid live directory, NUL-terminated relative path and correctly sized
        // open_how. No fallback weakens RESOLVE_BENEATH/NO_SYMLINKS/NO_MAGICLINKS.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.directory.as_raw_fd(),
                path.as_ptr(),
                &how,
                core::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd as i32) })
    }
}
