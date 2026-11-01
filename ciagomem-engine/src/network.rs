//! TLS-only bounded multiplexed reactor. Storage tickets never block the network owner.
use crate::{
    host::{AdmissionError, Client, Reply, Ticket},
    wire::{self, Header},
};
use ciagomem_core::{
    reactor::{Mode, Readiness},
    tls::{TlsContext, TlsStream},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
pub struct Config {
    pub bind: SocketAddr,
    pub tls: Arc<TlsContext>,
    pub principals: BTreeMap<[u8; 32], u32>,
    pub boot: u128,
    pub audience: u64,
    pub mode: Mode,
    pub cpu: Option<usize>,
    pub connections: usize,
    pub streams: usize,
    pub frame_bytes: usize,
    pub memory_bytes: usize,
    pub handshake_timeout: Duration,
    pub idle_timeout: Duration,
    pub drain_timeout: Duration,
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if self.boot == 0
            || self.connections == 0
            || self.connections > 256
            || self.streams == 0
            || self.streams > 128
            || self.frame_bytes < 65536
            || self.frame_bytes > 1 << 20
            || self.memory_bytes < self.frame_bytes * 4
            || self.principals.is_empty()
            || self.principals.len() > 4096
            || self.handshake_timeout.is_zero()
            || self.idle_timeout.is_zero()
            || self.drain_timeout.is_zero()
            || (self.mode == Mode::Uring && self.cpu.is_none())
            || (self.mode == Mode::Portable && !self.bind.ip().is_loopback())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid reactor configuration",
            ));
        }
        Ok(())
    }
}
struct Credit {
    used: Arc<AtomicUsize>,
    bytes: usize,
}
impl Credit {
    fn reserve(used: &Arc<AtomicUsize>, bytes: usize, limit: usize) -> Option<Self> {
        used.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(bytes).filter(|n| *n <= limit)
        })
        .ok()
        .map(|_| Self {
            used: used.clone(),
            bytes,
        })
    }
}
impl Drop for Credit {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
struct Pending {
    op: u8,
    ticket: Ticket,
    deadline: Instant,
    credit: Credit,
}
struct Outgoing {
    stream: u32,
    bytes: Vec<u8>,
    offset: usize,
    _reply: Option<Reply>,
    _credit: Option<Credit>,
}
struct Connection {
    tls: TlsStream,
    uid: Option<u32>,
    created: Instant,
    last: Instant,
    frame_started: Option<Instant>,
    input: Vec<u8>,
    consumed: usize,
    active: BTreeSet<u32>,
    pending: BTreeMap<u32, Pending>,
    out: VecDeque<Outgoing>,
    _credit: Credit,
}
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}
enum Incoming {
    Tcp(TcpListener),
    Connected(std::sync::mpsc::Receiver<std::os::unix::net::UnixStream>),
}
impl Incoming {
    fn fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        match self {
            Self::Tcp(listener) => Some(listener.as_raw_fd()),
            _ => None,
        }
    }
    fn accept(&self, tls: &Arc<TlsContext>) -> io::Result<TlsStream> {
        match self {
            Self::Tcp(l) => tls.session(l.accept()?.0, None),
            Self::Connected(receiver) => match receiver.try_recv() {
                Ok(stream) => tls.unix_session(stream, None),
                Err(_) => Err(io::ErrorKind::WouldBlock.into()),
            },
        }
    }
}
impl Server {
    pub fn spawn(client: Client, config: Config) -> io::Result<Self> {
        config.validate()?;
        let listener = TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        Self::start(client, config, Incoming::Tcp(listener), address)
    }
    /// Supervised connected streams use exactly the same TLS/authentication/frame reactor.
    pub fn spawn_connected(
        client: Client,
        config: Config,
    ) -> io::Result<(
        Self,
        std::sync::mpsc::SyncSender<std::os::unix::net::UnixStream>,
    )> {
        config.validate()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(config.connections);
        let address = config.bind;
        Ok((
            Self::start(client, config, Incoming::Connected(receiver), address)?,
            sender,
        ))
    }
    fn start(
        client: Client,
        config: Config,
        listener: Incoming,
        address: SocketAddr,
    ) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let shutdown = stop.clone();
        let (ready, receive) = std::sync::mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("ciagomem-network".into())
            .spawn(move || {
                if let Some(cpu) = config.cpu {
                    if let Err(error) = ciagomem_core::reactor::pin(cpu) {
                        let _ = ready.send(Err(error));
                        return Ok(());
                    }
                }
                let mut poller = match Readiness::new(config.mode, config.connections + 1) {
                    Ok(p) => p,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return Ok(());
                    }
                };
                if let Some(fd) = listener.fd() {
                    poller.set(0, fd, false)?;
                }
                let _ = ready.send(Ok(()));
                let memory = Arc::new(AtomicUsize::new(0));
                let mut connections: BTreeMap<u64, Connection> = BTreeMap::new();
                let mut next = 1u64;
                let mut draining = None;
                loop {
                    if shutdown.load(Ordering::Acquire) && draining.is_none() {
                        draining = Some(Instant::now());
                        poller.remove(0)?;
                    }
                    if draining.is_none() {
                        for _ in 0..16 {
                            match listener.accept(&config.tls) {
                                Ok(tls) => {
                                    if connections.len() >= config.connections {
                                        continue;
                                    }
                                    let Some(credit) = Credit::reserve(
                                        &memory,
                                        config.frame_bytes + 196608,
                                        config.memory_bytes,
                                    ) else {
                                        continue;
                                    };
                                    let now = Instant::now();
                                    let id = next;
                                    next = next.checked_add(1).ok_or_else(|| {
                                        io::Error::other("connection generation exhausted")
                                    })?;
                                    connections.insert(
                                        id,
                                        Connection {
                                            tls,
                                            uid: None,
                                            created: now,
                                            last: now,
                                            frame_started: None,
                                            input: Vec::new(),
                                            consumed: 0,
                                            active: BTreeSet::new(),
                                            pending: BTreeMap::new(),
                                            out: VecDeque::new(),
                                            _credit: credit,
                                        },
                                    );
                                }
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    let mut remove = Vec::new();
                    for (&id, connection) in &mut connections {
                        if draining.is_some_and(|start| start.elapsed() >= config.drain_timeout) {
                            remove.push(id);
                            continue;
                        }
                        match drive(connection, &client, &config, &memory, draining.is_some()) {
                            Ok(true) => {
                                if draining.is_some()
                                    && connection.pending.is_empty()
                                    && connection.out.is_empty()
                                {
                                    remove.push(id);
                                } else {
                                    poller.set(
                                        id,
                                        connection.tls.raw_fd(),
                                        connection.tls.wants_write() || !connection.out.is_empty(),
                                    )?;
                                }
                            }
                            _ => remove.push(id),
                        }
                    }
                    for id in remove {
                        poller.remove(id)?;
                        connections.remove(&id);
                    }
                    if draining.is_some() && connections.is_empty() {
                        return Ok(());
                    }
                    // Timed readiness also polls completed storage tickets, without waiting on a ticket.
                    poller.wait(1)?;
                    if draining.is_none() {
                        if let Some(fd) = listener.fd() {
                            poller.set(0, fd, false)?;
                        }
                    }
                }
            })?;
        match receive.recv() {
            Ok(Ok(())) => Ok(Self {
                address,
                stop,
                thread: Some(thread),
            }),
            other => {
                let _ = thread.join();
                match other {
                    Ok(Err(error)) => Err(error),
                    _ => Err(io::Error::other("reactor failed before readiness")),
                }
            }
        }
    }
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| io::Error::other("reactor panicked"))?
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.stop.store(true, Ordering::Release);
            let _ = thread.join();
        }
    }
}
fn queue(
    connection: &mut Connection,
    stream: u32,
    bytes: Vec<u8>,
    reply: Option<Reply>,
    credit: Option<Credit>,
) {
    connection.out.push_back(Outgoing {
        stream,
        bytes,
        offset: 0,
        _reply: reply,
        _credit: credit,
    });
}
fn drive(
    connection: &mut Connection,
    client: &Client,
    config: &Config,
    memory: &Arc<AtomicUsize>,
    draining: bool,
) -> io::Result<bool> {
    if connection.created.elapsed() > config.handshake_timeout && connection.uid.is_none() {
        return Ok(false);
    }
    if !connection.tls.handshake()? {
        return Ok(true);
    }
    if connection.uid.is_none() {
        connection.uid = config
            .principals
            .get(&connection.tls.peer_fingerprint()?)
            .copied();
        if connection.uid.is_none() {
            return Ok(false);
        }
    }
    if connection.last.elapsed() > config.idle_timeout
        || connection
            .frame_started
            .is_some_and(|s| s.elapsed() > config.handshake_timeout)
    {
        return Ok(false);
    }
    let mut completed = Vec::new();
    for (&stream, pending) in &connection.pending {
        match pending.ticket.try_receive() {
            Ok(Some(reply)) => completed.push((stream, Some(reply))),
            Ok(None) if Instant::now() < pending.deadline => {}
            _ => completed.push((stream, None)),
        }
    }
    for (stream, reply) in completed {
        let pending = connection.pending.remove(&stream).unwrap();
        let frame = if let Some(reply) = &reply {
            wire::encode_output(
                pending.op,
                stream,
                config.boot,
                &reply.result,
                config.frame_bytes,
            )
            .or_else(|error| {
                wire::response(
                    pending.op,
                    stream,
                    config.boot,
                    crate::retry::status(&error),
                    0,
                    0,
                    &[],
                    config.frame_bytes,
                )
            })
        } else {
            wire::response(
                pending.op,
                stream,
                config.boot,
                13,
                0,
                0,
                &[],
                config.frame_bytes,
            )
        }
        .map_err(|_| io::Error::other("response limit"))?;
        queue(connection, stream, frame, reply, Some(pending.credit));
    }
    if !draining {
        for _ in 0..4 {
            let mut bytes = [0; 16384];
            match connection.tls.read(&mut bytes) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    if connection.input.len().saturating_add(n)
                        > config.frame_bytes + wire::HEADER + 16384
                    {
                        return Ok(false);
                    }
                    if connection.input.len() == connection.consumed {
                        connection.frame_started = Some(Instant::now());
                    }
                    connection.input.extend(&bytes[..n]);
                    connection.last = Instant::now();
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => return Ok(false),
            }
        }
        for _ in 0..16 {
            let available = &connection.input[connection.consumed..];
            if available.len() < wire::HEADER {
                break;
            }
            let header = Header::decode(&available[..wire::HEADER], config.frame_bytes)
                .map_err(|_| io::Error::other("invalid frame"))?;
            if header.flags != 0
                || connection.active.contains(&header.stream)
                || connection.active.len() >= config.streams
            {
                return Ok(false);
            }
            if available.len() < wire::HEADER + header.length {
                break;
            }
            connection.active.insert(header.stream);
            if header.op == 1 {
                if header.length != 0 {
                    return Ok(false);
                }
                let mut hello = Vec::new();
                hello.extend(config.audience.to_le_bytes());
                hello.extend((config.frame_bytes as u32).to_le_bytes());
                hello.extend((config.streams as u32).to_le_bytes());
                hello.extend(0x0fu64.to_le_bytes());
                let bytes = wire::response(
                    1,
                    header.stream,
                    config.boot,
                    0,
                    4,
                    0,
                    &hello,
                    config.frame_bytes,
                )
                .unwrap();
                queue(connection, header.stream, bytes, None, None);
            } else {
                let request = wire::decode_request(
                    header,
                    &available[wire::HEADER..wire::HEADER + header.length],
                    connection.uid.unwrap(),
                    config.boot,
                );
                let credit =
                    Credit::reserve(memory, config.frame_bytes + 1024, config.memory_bytes);
                match (request, credit) {
                    (Ok(request), Some(credit)) => {
                        let deadline = connection.frame_started.unwrap_or_else(Instant::now)
                            + Duration::from_millis(request.timeout_ms as u64);
                        if Instant::now() >= deadline {
                            let bytes = wire::response(
                                header.op,
                                header.stream,
                                config.boot,
                                8,
                                0,
                                0,
                                &[],
                                config.frame_bytes,
                            )
                            .unwrap();
                            queue(connection, header.stream, bytes, None, None);
                        } else {
                            match client.submit(request.operation, request.token, request.peer) {
                                Ok(ticket) => {
                                    connection.pending.insert(
                                        header.stream,
                                        Pending {
                                            op: header.op,
                                            ticket,
                                            deadline,
                                            credit,
                                        },
                                    );
                                }
                                Err(error) => {
                                    let status = match error {
                                        AdmissionError::Permission => 3,
                                        AdmissionError::Invalid => 6,
                                        _ => 8,
                                    };
                                    let bytes = wire::response(
                                        header.op,
                                        header.stream,
                                        config.boot,
                                        status,
                                        0,
                                        0,
                                        &[],
                                        config.frame_bytes,
                                    )
                                    .unwrap();
                                    queue(connection, header.stream, bytes, None, None);
                                }
                            }
                        }
                    }
                    (Err(error), _) => {
                        let bytes = wire::response(
                            header.op,
                            header.stream,
                            config.boot,
                            crate::retry::status(&error),
                            0,
                            0,
                            &[],
                            config.frame_bytes,
                        )
                        .unwrap();
                        queue(connection, header.stream, bytes, None, None);
                    }
                    (_, None) => {
                        let bytes = wire::response(
                            header.op,
                            header.stream,
                            config.boot,
                            8,
                            0,
                            0,
                            &[],
                            config.frame_bytes,
                        )
                        .unwrap();
                        queue(connection, header.stream, bytes, None, None);
                    }
                }
            }
            connection.consumed += wire::HEADER + header.length;
            connection.frame_started = if connection.consumed < connection.input.len() {
                Some(Instant::now())
            } else {
                None
            };
        }
        if connection.consumed > 0 {
            connection.input.drain(..connection.consumed);
            connection.consumed = 0;
        }
    }
    let mut budget = 65536;
    while budget > 0 {
        let Some(frame) = connection.out.front_mut() else {
            break;
        };
        let end = frame.bytes.len().min(frame.offset + budget);
        match connection.tls.write(&frame.bytes[frame.offset..end]) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                frame.offset += n;
                budget -= n;
                connection.last = Instant::now();
                if frame.offset == frame.bytes.len() {
                    let frame = connection.out.pop_front().unwrap();
                    connection.active.remove(&frame.stream);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}
