//! TLS actor client with a crash-safe, single-pending-intent identity journal.
use crate::{
    retry::{WriteCommand, command_digest},
    wire::{self, Header},
    *,
};
use ciagomem_core::{
    Caveat, Macaroon,
    envelope::crc32c,
    reactor::{Mode, Readiness},
    tls::{TlsContext, TlsStream},
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid client state/frame")
}
fn extension(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}
#[derive(Clone)]
struct State {
    client: u64,
    next: u64,
    pending: Vec<u8>,
    last: Vec<u8>,
}
pub struct IdentityFile {
    path: PathBuf,
    lock: File,
    state: State,
    poisoned: bool,
}
impl Drop for IdentityFile {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}
impl IdentityFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(extension(path, ".lock"))?;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(e) => return Err(io::Error::other(format!("identity lock: {e}"))),
            }
        }
        let data = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path);
        let state = match data {
            Ok(mut file) => {
                let meta = file.metadata()?;
                if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.len() > 2 << 20 {
                    return Err(invalid());
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                decode_state(&bytes)?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let client = loop {
                    let n = ciagomem_core::random_id()? as u64;
                    if n != 0 {
                        break n;
                    }
                };
                let state = State {
                    client,
                    next: 1,
                    pending: Vec::new(),
                    last: Vec::new(),
                };
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)?;
                file.write_all(&encode_state(&state)?)?;
                file.sync_all()?;
                sync_parent(path)?;
                state
            }
            Err(e) => return Err(e),
        };
        let pending = extension(path, ".pending");
        match std::fs::remove_file(pending) {
            Ok(()) => sync_parent(path)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(Self {
            path: path.into(),
            lock,
            state,
            poisoned: false,
        })
    }
    pub fn client_id(&self) -> u64 {
        self.state.client
    }
    pub fn next_sequence(&self) -> u64 {
        self.state.next
    }
    pub fn has_pending(&self) -> bool {
        !self.state.pending.is_empty()
    }
    pub fn last_result(&self) -> Option<&[u8]> {
        (!self.state.last.is_empty()).then_some(self.state.last.as_slice())
    }
    fn save(&mut self, state: State) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::other("identity journal requires reopen"));
        }
        let result = (|| {
            let bytes = encode_state(&state)?;
            let temporary = extension(&self.path, ".pending");
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
            std::fs::rename(temporary, &self.path)?;
            sync_parent(&self.path)
        })();
        if result.is_err() {
            self.poisoned = true;
            return result;
        }
        self.state = state;
        Ok(())
    }
    fn prepare(&mut self, pending: Vec<u8>) -> io::Result<()> {
        if self.has_pending() {
            if self.state.pending != pending {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "resolve the existing pending intent",
                ));
            }
            return Ok(());
        }
        let mut state = self.state.clone();
        state.pending = pending;
        self.save(state)
    }
    fn complete(&mut self, response: Vec<u8>) -> io::Result<()> {
        let mut state = self.state.clone();
        state.next = state.next.checked_add(1).ok_or_else(invalid)?;
        state.pending.clear();
        state.last = response;
        self.save(state)
    }
}
fn sync_parent(path: &Path) -> io::Result<()> {
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()
}
fn encode_state(s: &State) -> io::Result<Vec<u8>> {
    if s.pending.len() > 1 << 20 || s.last.len() > 65536 {
        return Err(invalid());
    }
    let mut b = b"CIDFILE1".to_vec();
    b.extend(s.client.to_le_bytes());
    b.extend(s.next.to_le_bytes());
    b.extend((s.pending.len() as u32).to_le_bytes());
    b.extend((s.last.len() as u32).to_le_bytes());
    b.extend(&s.pending);
    b.extend(&s.last);
    let crc = crc32c(&[&b]);
    b.extend(crc.to_le_bytes());
    Ok(b)
}
fn decode_state(b: &[u8]) -> io::Result<State> {
    if b.len() < 36
        || &b[..8] != b"CIDFILE1"
        || crc32c(&[&b[..b.len() - 4]]) != u32::from_le_bytes(b[b.len() - 4..].try_into().unwrap())
    {
        return Err(invalid());
    }
    let client = u64::from_le_bytes(b[8..16].try_into().unwrap());
    let next = u64::from_le_bytes(b[16..24].try_into().unwrap());
    let pending = u32::from_le_bytes(b[24..28].try_into().unwrap()) as usize;
    let last = u32::from_le_bytes(b[28..32].try_into().unwrap()) as usize;
    if client == 0
        || next == 0
        || pending > 1 << 20
        || last > 65536
        || 32 + pending + last + 4 != b.len()
    {
        return Err(invalid());
    }
    Ok(State {
        client,
        next,
        pending: b[32..32 + pending].to_vec(),
        last: b[32 + pending..b.len() - 4].to_vec(),
    })
}
pub struct Connection {
    tls: TlsStream,
    poller: Readiness,
    boot: u128,
    audience: u64,
    max_frame: usize,
    stream: u32,
    timeout: Duration,
    poisoned: bool,
}
pub struct Response {
    pub status: u16,
    pub kind: u8,
    pub sequence: u64,
    pub boot: u128,
    pub body: Vec<u8>,
}
impl Connection {
    pub fn connect(
        address: SocketAddr,
        name: &str,
        tls: Arc<TlsContext>,
        timeout: Duration,
    ) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(invalid());
        }
        let socket = TcpStream::connect_timeout(&address, timeout)?;
        Self::from_stream(tls.session(socket, Some(name))?, timeout)
    }
    pub fn from_unix_stream(
        socket: std::os::unix::net::UnixStream,
        name: &str,
        tls: Arc<TlsContext>,
        timeout: Duration,
    ) -> io::Result<Self> {
        Self::from_stream(tls.unix_session(socket, Some(name))?, timeout)
    }
    fn from_stream(mut stream: TlsStream, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(invalid());
        }
        let mut poller = Readiness::new(Mode::Portable, 1)?;
        let deadline = Instant::now() + timeout;
        while !stream.handshake()? {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            poller.set(1, stream.raw_fd(), stream.wants_write())?;
            poller.wait(1)?;
        }
        let mut connection = Self {
            tls: stream,
            poller,
            boot: 0,
            audience: 0,
            max_frame: 1 << 20,
            stream: 0,
            timeout,
            poisoned: false,
        };
        let hello = connection.exchange(1, &[])?;
        if hello.status != 0 || hello.kind != 4 || hello.body.len() != 24 {
            return Err(invalid());
        }
        connection.boot = hello.boot;
        connection.audience = u64::from_le_bytes(hello.body[..8].try_into().unwrap());
        connection.max_frame = u32::from_le_bytes(hello.body[8..12].try_into().unwrap()) as usize;
        if connection.max_frame < 65536
            || connection.max_frame > 1 << 20
            || u64::from_le_bytes(hello.body[16..24].try_into().unwrap()) & 15 != 15
        {
            return Err(invalid());
        }
        Ok(connection)
    }
    pub fn boot_epoch(&self) -> u128 {
        self.boot
    }
    pub fn audience(&self) -> u64 {
        self.audience
    }
    fn wait(&mut self, writing: bool, deadline: Instant) -> io::Result<()> {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.poller
            .set(1, self.tls.raw_fd(), writing || self.tls.wants_write())?;
        self.poller.wait(1)?;
        Ok(())
    }
    pub fn exchange(&mut self, op: u8, payload: &[u8]) -> io::Result<Response> {
        if self.poisoned {
            return Err(io::Error::other("reconnect after an incomplete exchange"));
        }
        self.poisoned = true;
        let result = self.exchange_inner(op, payload);
        if result.is_ok() {
            self.poisoned = false;
        }
        result
    }
    fn exchange_inner(&mut self, op: u8, payload: &[u8]) -> io::Result<Response> {
        if payload.len() > self.max_frame {
            return Err(invalid());
        }
        self.stream = self.stream.checked_add(1).ok_or_else(invalid)?;
        let id = self.stream;
        let frame = Header {
            op,
            flags: 0,
            stream: id,
            length: payload.len(),
        }
        .frame(payload)
        .map_err(|_| invalid())?;
        let deadline = Instant::now() + self.timeout;
        let mut offset = 0;
        while offset < frame.len() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.tls.write(&frame[offset..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(true, deadline)?,
                Err(e) => return Err(e),
            }
        }
        let mut header = [0; wire::HEADER];
        self.read_exact_until(&mut header, deadline)?;
        let header = Header::decode(&header, self.max_frame).map_err(|_| invalid())?;
        if header.op != op
            || header.stream != id
            || header.flags & 1 == 0
            || header.length < wire::RESPONSE
        {
            return Err(invalid());
        }
        let mut payload = vec![0; header.length];
        self.read_exact_until(&mut payload, deadline)?;
        let status = u16::from_le_bytes(payload[..2].try_into().unwrap());
        let kind = payload[3];
        let len = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
        let sequence = u64::from_le_bytes(payload[8..16].try_into().unwrap());
        let boot = u128::from_le_bytes(payload[16..32].try_into().unwrap());
        if payload[2] != u8::from(status == 0 || kind == 1)
            || boot == 0
            || kind > 4
            || status > 16
            || len + wire::RESPONSE != payload.len()
            || (self.boot != 0 && boot != self.boot)
            || ((header.flags & 2 != 0) != (status != 0))
        {
            return Err(invalid());
        }
        Ok(Response {
            status,
            kind,
            sequence,
            boot,
            body: payload[wire::RESPONSE..].to_vec(),
        })
    }
    fn read_exact_until(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
        let mut offset = 0;
        while offset < bytes.len() {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.tls.read(&mut bytes[offset..]) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => offset += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(false, deadline)?,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    pub fn payload(
        &self,
        token: &Macaroon,
        key: &str,
        incarnation: u128,
        sequence: u64,
        body: &[u8],
    ) -> io::Result<Vec<u8>> {
        let token_bytes = token.to_bytes().map_err(|_| invalid())?;
        let tenant = token
            .caveats
            .iter()
            .find_map(|c| {
                if let Caveat::Tenant(v) = c {
                    Some(*v)
                } else {
                    None
                }
            })
            .ok_or_else(invalid)?;
        let client = token
            .caveats
            .iter()
            .find_map(|c| {
                if let Caveat::ClientId(v) = c {
                    Some(*v)
                } else {
                    None
                }
            })
            .ok_or_else(invalid)?;
        if key.len() > 1024 || body.len() > self.max_frame {
            return Err(invalid());
        }
        let mut p = vec![0; wire::PREFIX];
        p[0] = 3;
        p[1] = 1;
        p[2..4].copy_from_slice(&(token_bytes.len() as u16).to_le_bytes());
        p[12..16].copy_from_slice(&(key.len() as u32).to_le_bytes());
        p[16..24].copy_from_slice(&client.to_le_bytes());
        p[24..32].copy_from_slice(&sequence.to_le_bytes());
        p[32..48].copy_from_slice(&tenant.to_le_bytes());
        p[48..64].copy_from_slice(&self.boot.to_le_bytes());
        p[64..80].copy_from_slice(&incarnation.to_le_bytes());
        p[80..84].copy_from_slice(&(self.timeout.as_millis() as u32).to_le_bytes());
        p[84..88].copy_from_slice(&(body.len() as u32).to_le_bytes());
        p.extend(token_bytes);
        p.extend(key.as_bytes());
        p.extend(body);
        if p.len() > self.max_frame {
            return Err(invalid());
        }
        Ok(p)
    }
    pub fn read(
        &mut self,
        token: &Macaroon,
        actor: ActorRef,
        minimum: u64,
    ) -> io::Result<Response> {
        let mut body = vec![1];
        body.extend(minimum.to_le_bytes());
        let p = self.payload(token, &actor.id.to_string(), actor.incarnation, 0, &body)?;
        self.exchange(6, &p)
    }
    pub fn query(&mut self, token: &Macaroon, text: &str) -> io::Result<Response> {
        if text.len() > 65536 {
            return Err(invalid());
        }
        let mut body = vec![1];
        body.extend((text.len() as u32).to_le_bytes());
        body.extend(text.as_bytes());
        let p = self.payload(token, "", 0, 0, &body)?;
        self.exchange(9, &p)
    }
    /// Persist before sending. On timeout/disconnect keep the identity file and reconnect/resume.
    pub fn write(
        &mut self,
        identity: &mut IdentityFile,
        token: &Macaroon,
        command: WriteCommand,
    ) -> io::Result<Response> {
        self.prepare_write(identity, token, &command)?;
        self.resume(identity, token)
    }
    /// Durably stage an intent without sending it. Resume with renewed auth after reconnect/restart.
    pub fn prepare_write(
        &self,
        identity: &mut IdentityFile,
        token: &Macaroon,
        command: &WriteCommand,
    ) -> io::Result<()> {
        let (op, key, incarnation, body) = encode_command(command)?;
        let p = self.payload(token, &key, incarnation, identity.next_sequence(), &body)?;
        let parsed = wire::decode_request(
            Header {
                op,
                flags: 0,
                stream: 1,
                length: p.len(),
            },
            &p,
            0,
            self.boot,
        )
        .map_err(|_| invalid())?;
        let crate::host::Operation::WriteRetry {
            key: request_key, ..
        } = parsed.operation
        else {
            return Err(invalid());
        };
        if request_key.client != identity.client_id() {
            return Err(invalid());
        }
        let mut pending = vec![op];
        pending.extend(request_key.tenant.to_le_bytes());
        pending.extend(incarnation.to_le_bytes());
        pending.extend((key.len() as u16).to_le_bytes());
        pending.extend(command_digest(command));
        pending.extend(key.as_bytes());
        pending.extend(body);
        identity.prepare(pending)
    }
    pub fn resume(
        &mut self,
        identity: &mut IdentityFile,
        token: &Macaroon,
    ) -> io::Result<Response> {
        let saved = identity.state.pending.clone();
        if saved.len() < 67 {
            return Err(invalid());
        }
        let op = saved[0];
        let tenant = u128::from_le_bytes(saved[1..17].try_into().unwrap());
        let incarnation = u128::from_le_bytes(saved[17..33].try_into().unwrap());
        let key_len = u16::from_le_bytes(saved[33..35].try_into().unwrap()) as usize;
        let digest: &[u8] = &saved[35..67];
        if saved.len() < 67 + key_len {
            return Err(invalid());
        }
        let key = std::str::from_utf8(&saved[67..67 + key_len]).map_err(|_| invalid())?;
        let body = &saved[67 + key_len..];
        let p = self.payload(token, key, incarnation, identity.next_sequence(), body)?;
        if u128::from_le_bytes(p[32..48].try_into().unwrap()) != tenant
            || u64::from_le_bytes(p[16..24].try_into().unwrap()) != identity.client_id()
        {
            return Err(invalid());
        }
        let response = self.exchange(op, &p)?;
        if response.kind == 1 {
            if response.body.len() < 64
                || response.body[..16] != tenant.to_le_bytes()
                || response.body[16..24] != identity.client_id().to_le_bytes()
                || response.body[24..32] != identity.next_sequence().to_le_bytes()
                || response.body[32..64] != *digest
            {
                return Err(invalid());
            }
            let mut result = Vec::new();
            result.extend(response.status.to_le_bytes());
            result.extend(response.sequence.to_le_bytes());
            result.extend(&response.body);
            identity.complete(result)?;
        }
        Ok(response)
    }
}
pub fn encode_command(command: &WriteCommand) -> io::Result<(u8, String, u128, Vec<u8>)> {
    let mut body = vec![1];
    let (op, key, incarnation) = match command {
        WriteCommand::Create(r) => {
            let bytes = image::encode(r);
            if bytes.len() > 65344 {
                return Err(invalid());
            }
            body.extend((bytes.len() as u32).to_le_bytes());
            body.extend(bytes);
            (4, String::new(), 0)
        }
        WriteCommand::Replace {
            record,
            expected_version,
        } => {
            body.extend(expected_version.to_le_bytes());
            let bytes = image::encode(record);
            if bytes.len() > 65344 {
                return Err(invalid());
            }
            body.extend((bytes.len() as u32).to_le_bytes());
            body.extend(bytes);
            (5, record.actor.id.to_string(), record.actor.incarnation)
        }
        WriteCommand::Mutate {
            actor,
            expected_version,
            query,
        } => {
            if query.len() > 65536 {
                return Err(invalid());
            }
            body.extend(expected_version.to_le_bytes());
            body.extend((query.len() as u32).to_le_bytes());
            body.extend(query.as_bytes());
            (7, actor.id.to_string(), actor.incarnation)
        }
        WriteCommand::Delete {
            actor,
            expected_version,
        } => {
            body.extend(expected_version.to_le_bytes());
            (8, actor.id.to_string(), actor.incarnation)
        }
    };
    Ok((op, key, incarnation, body))
}
