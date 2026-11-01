//! Nonblocking TLS1.3/mTLS over system OpenSSL 3.5 LTS (>=3.5.9). No custom cryptographic algorithms.
use std::{
    ffi::{CString, c_char, c_int, c_long, c_void},
    io,
    net::TcpStream,
    os::fd::AsRawFd,
    path::Path,
    ptr::{self, NonNull},
    sync::Arc,
};
#[allow(non_camel_case_types)]
enum SSL_CTX {}
#[allow(non_camel_case_types)]
enum SSL {}
#[allow(non_camel_case_types)]
enum SSL_METHOD {}
enum X509 {}
#[link(name = "ssl")]
#[link(name = "crypto")]
unsafe extern "C" {
    fn OpenSSL_version_num() -> std::ffi::c_ulong;
    fn TLS_server_method() -> *const SSL_METHOD;
    fn TLS_client_method() -> *const SSL_METHOD;
    fn SSL_CTX_new(method: *const SSL_METHOD) -> *mut SSL_CTX;
    fn SSL_CTX_free(ctx: *mut SSL_CTX);
    fn SSL_CTX_ctrl(ctx: *mut SSL_CTX, cmd: c_int, value: c_long, arg: *mut c_void) -> c_long;
    fn SSL_CTX_set_verify(
        ctx: *mut SSL_CTX,
        mode: c_int,
        callback: Option<unsafe extern "C" fn(c_int, *mut c_void) -> c_int>,
    );
    fn SSL_CTX_use_certificate_chain_file(ctx: *mut SSL_CTX, path: *const c_char) -> c_int;
    fn SSL_CTX_use_PrivateKey_file(ctx: *mut SSL_CTX, path: *const c_char, kind: c_int) -> c_int;
    fn SSL_CTX_check_private_key(ctx: *const SSL_CTX) -> c_int;
    fn SSL_CTX_load_verify_locations(
        ctx: *mut SSL_CTX,
        ca: *const c_char,
        dir: *const c_char,
    ) -> c_int;
    fn SSL_CTX_set_max_early_data(ctx: *mut SSL_CTX, max: u32) -> c_int;
    fn SSL_CTX_set_verify_depth(ctx: *mut SSL_CTX, depth: c_int);
    fn SSL_CTX_set_num_tickets(ctx: *mut SSL_CTX, num: usize) -> c_int;
    fn SSL_new(ctx: *mut SSL_CTX) -> *mut SSL;
    fn SSL_free(ssl: *mut SSL);
    fn SSL_set_fd(ssl: *mut SSL, fd: c_int) -> c_int;
    fn SSL_set1_host(ssl: *mut SSL, name: *const c_char) -> c_int;
    fn SSL_ctrl(ssl: *mut SSL, cmd: c_int, value: c_long, arg: *mut c_void) -> c_long;
    fn SSL_accept(ssl: *mut SSL) -> c_int;
    fn SSL_connect(ssl: *mut SSL) -> c_int;
    fn SSL_get_error(ssl: *const SSL, result: c_int) -> c_int;
    fn SSL_read_ex(ssl: *mut SSL, out: *mut c_void, len: usize, read: *mut usize) -> c_int;
    fn SSL_write_ex(ssl: *mut SSL, input: *const c_void, len: usize, written: *mut usize) -> c_int;
    fn SSL_get_verify_result(ssl: *const SSL) -> c_long;
    fn SSL_get1_peer_certificate(ssl: *const SSL) -> *mut X509;
    fn i2d_X509(cert: *const X509, out: *mut *mut u8) -> c_int;
    fn X509_free(cert: *mut X509);
    fn ERR_clear_error();
}
fn failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "TLS authentication/protocol failure",
    )
}
fn cpath(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))
}
pub struct TlsContext {
    ctx: NonNull<SSL_CTX>,
    server: bool,
}
// SAFETY: OpenSSL3 contexts are thread-safe; construction completes before publication,
// all later SSL sessions have independent state and this API never mutates the shared context.
unsafe impl Send for TlsContext {}
unsafe impl Sync for TlsContext {}
impl Drop for TlsContext {
    fn drop(&mut self) {
        unsafe { SSL_CTX_free(self.ctx.as_ptr()) }
    }
}
impl TlsContext {
    pub fn new(cert: &Path, key: &Path, ca: &Path, server: bool) -> io::Result<Arc<Self>> {
        // SAFETY: C strings outlive calls; ownership of each allocation is tracked below.
        unsafe {
            let version = OpenSSL_version_num();
            if version & 0xfff00000 != 0x30500000 || version < 0x30500090 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "OpenSSL 3.5 LTS >=3.5.9 required",
                ));
            }
            let context = Self {
                ctx: NonNull::new(SSL_CTX_new(if server {
                    TLS_server_method()
                } else {
                    TLS_client_method()
                }))
                .ok_or_else(failure)?,
                server,
            };
            let raw = context.ctx.as_ptr();
            if SSL_CTX_ctrl(raw, 123, 0x0304, ptr::null_mut()) != 1
                || SSL_CTX_ctrl(raw, 124, 0x0304, ptr::null_mut()) != 1
            {
                return Err(failure());
            }
            SSL_CTX_ctrl(raw, 51, 65536, ptr::null_mut());
            SSL_CTX_set_verify(raw, if server { 3 } else { 1 }, None);
            SSL_CTX_set_verify_depth(raw, 8);
            if SSL_CTX_set_max_early_data(raw, 0) != 1 {
                return Err(failure());
            }
            if SSL_CTX_load_verify_locations(raw, cpath(ca)?.as_ptr(), ptr::null()) != 1
                || SSL_CTX_use_certificate_chain_file(raw, cpath(cert)?.as_ptr()) != 1
                || SSL_CTX_use_PrivateKey_file(raw, cpath(key)?.as_ptr(), 1) != 1
                || SSL_CTX_check_private_key(raw) != 1
            {
                return Err(failure());
            }
            if server && SSL_CTX_set_num_tickets(raw, 0) != 1 {
                return Err(failure());
            }
            Ok(Arc::new(context))
        }
    }
    pub fn session(
        self: &Arc<Self>,
        socket: TcpStream,
        hostname: Option<&str>,
    ) -> io::Result<TlsStream> {
        self.make_session(Socket::Tcp(socket), hostname)
    }
    pub fn unix_session(
        self: &Arc<Self>,
        socket: std::os::unix::net::UnixStream,
        hostname: Option<&str>,
    ) -> io::Result<TlsStream> {
        self.make_session(Socket::Unix(socket), hostname)
    }
    fn make_session(
        self: &Arc<Self>,
        socket: Socket,
        hostname: Option<&str>,
    ) -> io::Result<TlsStream> {
        socket.nonblocking()?;

        unsafe {
            let ssl = NonNull::new(SSL_new(self.ctx.as_ptr())).ok_or_else(failure)?;
            let stream = TlsStream {
                ssl,
                socket,
                context: self.clone(),
                ready: false,
                want_write: false,
                pending: None,
            };
            if SSL_set_fd(ssl.as_ptr(), stream.socket.as_raw_fd()) != 1 {
                return Err(failure());
            }
            if !self.server {
                let host = CString::new(hostname.ok_or_else(failure)?).map_err(|_| failure())?;
                if SSL_set1_host(ssl.as_ptr(), host.as_ptr()) != 1
                    || SSL_ctrl(ssl.as_ptr(), 55, 0, host.as_ptr() as *mut c_void) != 1
                {
                    return Err(failure());
                }
            }
            Ok(stream)
        }
    }
}
enum Socket {
    Tcp(TcpStream),
    Unix(std::os::unix::net::UnixStream),
}
impl Socket {
    fn nonblocking(&self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_nonblocking(true),
            Self::Unix(s) => s.set_nonblocking(true),
        }
    }
}
impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        match self {
            Self::Tcp(s) => s.as_raw_fd(),
            Self::Unix(s) => s.as_raw_fd(),
        }
    }
}
pub struct TlsStream {
    ssl: NonNull<SSL>,
    socket: Socket,
    context: Arc<TlsContext>,
    ready: bool,
    want_write: bool,
    pending: Option<Vec<u8>>,
}
impl Drop for TlsStream {
    fn drop(&mut self) {
        unsafe { SSL_free(self.ssl.as_ptr()) }
    }
}
impl TlsStream {
    fn classify(&mut self, result: c_int) -> io::Result<()> {
        let error = unsafe { SSL_get_error(self.ssl.as_ptr(), result) };
        match error {
            2 | 3 => {
                self.want_write = error == 3;
                Err(io::ErrorKind::WouldBlock.into())
            }
            6 => Err(io::ErrorKind::UnexpectedEof.into()),
            _ => Err(failure()),
        }
    }
    pub fn handshake(&mut self) -> io::Result<bool> {
        if self.ready {
            return Ok(true);
        }
        unsafe {
            ERR_clear_error();
        }
        let result = unsafe {
            if self.context.server {
                SSL_accept(self.ssl.as_ptr())
            } else {
                SSL_connect(self.ssl.as_ptr())
            }
        };
        if result == 1 {
            if unsafe { SSL_get_verify_result(self.ssl.as_ptr()) } != 0 {
                return Err(failure());
            }
            self.ready = true;
            self.want_write = false;
            Ok(true)
        } else {
            match self.classify(result) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
                other => other.map(|_| false),
            }
        }
    }
    pub fn peer_fingerprint(&self) -> io::Result<[u8; 32]> {
        if !self.ready {
            return Err(failure());
        }
        unsafe {
            let cert =
                NonNull::new(SSL_get1_peer_certificate(self.ssl.as_ptr())).ok_or_else(failure)?;
            let n = i2d_X509(cert.as_ptr(), ptr::null_mut());
            if n <= 0 || n > 65536 {
                X509_free(cert.as_ptr());
                return Err(failure());
            }
            let mut bytes = vec![0; n as usize];
            let mut output = bytes.as_mut_ptr();
            let actual = i2d_X509(cert.as_ptr(), &mut output);
            X509_free(cert.as_ptr());
            if actual != n {
                return Err(failure());
            }
            Ok(ciagomem_crypto::content_digest(&bytes))
        }
    }
    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.socket.as_raw_fd()
    }
    pub fn wants_write(&self) -> bool {
        self.want_write || self.pending.is_some()
    }
}
impl io::Read for TlsStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.ready {
            return Err(failure());
        }
        // A nonblocking SSL_write retry must retain its operation and exact plaintext.
        if self.pending.is_some() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if out.is_empty() {
            return Ok(0);
        }
        let mut n = 0;
        unsafe {
            ERR_clear_error();
        }
        let result = unsafe {
            SSL_read_ex(
                self.ssl.as_ptr(),
                out.as_mut_ptr().cast(),
                out.len(),
                &mut n,
            )
        };
        if result == 1 {
            self.want_write = false;
            Ok(n)
        } else {
            self.classify(result).map(|_| 0)
        }
    }
}
impl io::Write for TlsStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.ready {
            return Err(failure());
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let n = self
            .pending
            .as_ref()
            .map_or(bytes.len().min(16384), Vec::len);
        if bytes.len() < n {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if let Some(pending) = &self.pending {
            if pending.as_slice() != &bytes[..n] {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "TLS retry changed plaintext",
                ));
            }
        } else {
            self.pending = Some(bytes[..n].to_vec());
        }
        let pending = self.pending.as_ref().unwrap();
        let mut written = 0;
        unsafe {
            ERR_clear_error();
        }
        let result = unsafe {
            SSL_write_ex(
                self.ssl.as_ptr(),
                pending.as_ptr().cast(),
                pending.len(),
                &mut written,
            )
        };
        if result == 1 {
            self.pending = None;
            self.want_write = false;
            Ok(written)
        } else {
            self.classify(result).map(|_| 0)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
