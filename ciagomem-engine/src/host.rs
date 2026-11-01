//! Bounded single-owner application host. Trusted transport adapters supply peer identity.
//! The network reactor submits bounded tickets; disk completions stay on the storage owner.
use crate::{links::OnDelete, persistence::DurableJournal, *};
use ciagomem_core::{
    Clock, KeyedPrf, Macaroon, OP_OPERATOR, OP_READ, OP_WRITE, RequestCtx, VerifiedMacaroon,
};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Mutex, mpsc},
    thread,
};

pub enum Operation {
    Put(Record),
    Read {
        actor: ActorRef,
        minimum_version: u64,
    },
    WriteRetry {
        key: crate::retry::RequestKey,
        command: crate::retry::WriteCommand,
    },
    Query(String),
    ReadQuery(String),
    Delete(ActorRef),
    Link {
        dependent: ActorRef,
        target: ActorRef,
        mode: OnDelete,
    },
    BeginDelete(ActorRef),
    AdvanceDelete {
        job: u64,
        cursor: u64,
        max: usize,
    },
    Checkpoint,
}
pub enum Output {
    Unit,
    Document { snapshot: Snapshot, version: u64 },
    Receipt(crate::retry::Receipt),
    Query(QueryResult),
    Deleted(usize),
    DeleteJob(DeleteStatus),
}
/// Supplied by a trusted authenticated connection adapter, not decoded as asserted OS identity.
pub struct Peer {
    pub tenant: u128,
    pub session: u128,
    pub uid: u32,
}
pub struct Authentication {
    pub key: KeyedPrf,
    pub audience: u64,
    pub key_epoch: u64,
    pub boot_epoch: u128,
    pub clock: Arc<dyn Clock + Send + Sync>,
}
struct Verifier {
    config: Authentication,
    now: AtomicU64,
}
impl Verifier {
    fn verify(
        &self,
        token: &Macaroon,
        peer: &Peer,
        operation: &Operation,
    ) -> Result<VerifiedMacaroon> {
        let now = self.config.clock.now_ns();
        let now = now.max(self.now.fetch_max(now, Ordering::AcqRel));
        token
            .verify(
                &self.config.key,
                &RequestCtx {
                    tenant_incarnation: peer.tenant,
                    session: peer.session,
                    peer_uid: peer.uid,
                    audience: self.config.audience,
                    key_epoch: self.config.key_epoch,
                    boot_epoch: self.config.boot_epoch,
                    op_bit: match operation {
                        Operation::Query(_) | Operation::ReadQuery(_) | Operation::Read { .. } => {
                            OP_READ
                        }
                        Operation::WriteRetry { .. } => OP_READ | OP_WRITE,
                        Operation::Checkpoint => OP_OPERATOR,
                        _ => OP_WRITE,
                    },
                    now_ns: now,
                },
            )
            .map_err(|_| Error::Permission)
    }
}
pub struct HostLimits {
    pub requests: usize,
    pub bytes: usize,
    pub per_tenant_requests: usize,
    pub request_bytes: usize,
    pub response_bytes: usize,
}
impl Default for HostLimits {
    fn default() -> Self {
        Self {
            requests: 32,
            bytes: 64 << 20,
            per_tenant_requests: 8,
            request_bytes: 1 << 20,
            response_bytes: 1 << 20,
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum AdmissionError {
    Busy,
    Closed,
    Invalid,
    Permission,
}
struct State {
    accepting: bool,
    requests: usize,
    bytes: usize,
    tenants: BTreeMap<u128, usize>,
}
struct Reservation {
    state: Arc<Mutex<State>>,
    tenant: u128,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.requests -= 1;
            state.bytes -= self.bytes;
            if let Some(n) = state.tenants.get_mut(&self.tenant) {
                *n -= 1;
                if *n == 0 {
                    state.tenants.remove(&self.tenant);
                }
            }
        }
    }
}
pub struct Reply {
    pub result: Result<Output>,
    _reservation: Reservation,
}
pub struct Ticket(mpsc::Receiver<Reply>);
impl Ticket {
    pub fn try_receive(&self) -> std::result::Result<Option<Reply>, mpsc::TryRecvError> {
        match self.0.try_recv() {
            Ok(reply) => Ok(Some(reply)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn receive(self) -> std::result::Result<Reply, mpsc::RecvError> {
        self.0.recv()
    }
}
struct Request {
    operation: Operation,
    token: Macaroon,
    peer: Peer,
    reply: mpsc::SyncSender<Reply>,
    reservation: Reservation,
}
enum Message {
    Request(Request),
    Stop,
}
#[derive(Clone)]
pub struct Client {
    sender: mpsc::SyncSender<Message>,
    state: Arc<Mutex<State>>,
    limits: Arc<HostLimits>,
    verifier: Arc<Verifier>,
}
pub struct Host {
    client: Client,
    worker: Option<thread::JoinHandle<()>>,
}
impl Client {
    /// Busy means not accepted. Disconnect after acceptance does not imply rollback.
    pub fn submit(
        &self,
        operation: Operation,
        token: Macaroon,
        peer: Peer,
    ) -> std::result::Result<Ticket, AdmissionError> {
        if token.caveats.len() > 32 {
            return Err(AdmissionError::Invalid);
        }
        let token_bytes = token
            .caveats
            .iter()
            .try_fold(32usize, |n, c| {
                n.checked_add(match c {
                    ciagomem_core::Caveat::Unknown(v) => v.len(),
                    _ => 24,
                })
            })
            .ok_or(AdmissionError::Invalid)?;
        let body = match &operation {
            Operation::Put(record) => Limits::default()
                .validate(record)
                .map_err(|_| AdmissionError::Invalid)?,
            Operation::WriteRetry { command, .. } => match command {
                crate::retry::WriteCommand::Create(r)
                | crate::retry::WriteCommand::Replace { record: r, .. } => Limits::default()
                    .validate(r)
                    .map_err(|_| AdmissionError::Invalid)?,
                crate::retry::WriteCommand::Mutate { query, .. } => {
                    if query.len() > 65536 {
                        return Err(AdmissionError::Invalid);
                    }
                    query.len() + 128
                }
                _ => 128,
            },
            Operation::Query(text) | Operation::ReadQuery(text) => {
                if text.len() > 65536 {
                    return Err(AdmissionError::Invalid);
                }
                text.len()
            }
            _ => 128,
        };
        let input = body
            .checked_add(token_bytes)
            .ok_or(AdmissionError::Invalid)?;
        if input > self.limits.request_bytes {
            return Err(AdmissionError::Invalid);
        }
        self.verifier
            .verify(&token, &peer, &operation)
            .map_err(|_| AdmissionError::Permission)?;
        let bytes = input
            .checked_add(self.limits.response_bytes)
            .and_then(|n| n.checked_add(1024))
            .ok_or(AdmissionError::Invalid)?;
        let mut state = self.state.lock().map_err(|_| AdmissionError::Closed)?;
        if !state.accepting {
            return Err(AdmissionError::Closed);
        }
        if state.requests >= self.limits.requests
            || state.tenants.get(&peer.tenant).copied().unwrap_or(0)
                >= self.limits.per_tenant_requests
            || bytes > self.limits.bytes.saturating_sub(state.bytes)
        {
            return Err(AdmissionError::Busy);
        }
        state.requests += 1;
        state.bytes += bytes;
        *state.tenants.entry(peer.tenant).or_default() += 1;
        let reservation = Reservation {
            state: self.state.clone(),
            tenant: peer.tenant,
            bytes,
        };
        let (send, receive) = mpsc::sync_channel(1);
        let message = Message::Request(Request {
            operation,
            token,
            peer,
            reply: send,
            reservation,
        });
        // No blocking send while holding the admission lock.
        match self.sender.try_send(message) {
            Ok(()) => Ok(Ticket(receive)),
            Err(error) => {
                state.accepting = !matches!(error, mpsc::TrySendError::Disconnected(_));
                drop(state);
                drop(error);
                Err(AdmissionError::Busy)
            }
        }
    }
}
impl Host {
    pub fn spawn(
        engine: Engine<DurableJournal>,
        auth: Authentication,
        limits: HostLimits,
    ) -> std::io::Result<Self> {
        Self::spawn_with(move || Ok(engine), auth, limits)
    }
    /// Construct/recover on the storage thread so SINGLE_ISSUER rings retain their owner.
    /// No request client is returned until recovery and configuration validation succeed.
    pub fn spawn_with<F>(
        factory: F,
        auth: Authentication,
        limits: HostLimits,
    ) -> std::io::Result<Self>
    where
        F: FnOnce() -> Result<Engine<DurableJournal>> + Send + 'static,
    {
        if limits.requests > 65536
            || limits.request_bytes > 16 << 20
            || limits.response_bytes > 16 << 20
            || limits.requests == 0
            || limits.bytes == 0
            || limits.per_tenant_requests == 0
            || limits.request_bytes == 0
            || limits.response_bytes == 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid host limits",
            ));
        }
        let state = Arc::new(Mutex::new(State {
            accepting: true,
            requests: 0,
            bytes: 0,
            tenants: BTreeMap::new(),
        }));
        let (sender, receiver) = mpsc::sync_channel(limits.requests);
        let verifier = Arc::new(Verifier {
            config: auth,
            now: AtomicU64::new(0),
        });
        let client = Client {
            sender,
            state,
            limits: Arc::new(limits),
            verifier: verifier.clone(),
        };
        let worker_limits = client.limits.clone();
        let (ready, readiness) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("ciagomem-storage".into())
            .spawn(move || {
                let mut engine = match factory() {
                    Ok(engine)
                        if engine.boot_epoch == verifier.config.boot_epoch
                            && engine.limits.result_bytes <= worker_limits.response_bytes =>
                    {
                        engine
                    }
                    Ok(_) => {
                        let _ = ready.send(Err(Error::InvalidValue));
                        return;
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Ok(message) = receiver.recv() {
                    let Message::Request(request) = message else {
                        break;
                    };
                    let result = verifier
                        .verify(&request.token, &request.peer, &request.operation)
                        .and_then(|token| {
                            // execute() rechecks actual stage permissions; queued expiry is checked here.
                            match request.operation {
                                Operation::Put(r) => engine.put(r, &token).map(|_| Output::Unit),
                                Operation::Read {
                                    actor,
                                    minimum_version,
                                } => {
                                    let snapshot = engine.snapshot(actor, &token)?;
                                    let version = engine.document_version(actor, &token)?;
                                    if version < minimum_version {
                                        Err(Error::Budget)
                                    } else {
                                        Ok(Output::Document { snapshot, version })
                                    }
                                }
                                Operation::WriteRetry { key, command } => {
                                    let first = engine.execute_retry(key, command.clone(), &token);
                                    if matches!(first, Err(Error::Budget)) {
                                        engine.checkpoint()?;
                                        engine
                                            .execute_retry(key, command, &token)
                                            .map(Output::Receipt)
                                    } else {
                                        first.map(Output::Receipt)
                                    }
                                }
                                Operation::ReadQuery(text) => {
                                    let query =
                                        ciagomem_ql::parse(&text).map_err(|_| Error::Parse)?;
                                    if query.stages.iter().any(|s| {
                                        !matches!(
                                            s,
                                            ciagomem_ql::Stage::Filter(_)
                                                | ciagomem_ql::Stage::Similarity { .. }
                                                | ciagomem_ql::Stage::Project(_)
                                                | ciagomem_ql::Stage::GroupBy(_)
                                                | ciagomem_ql::Stage::Reduce(_)
                                                | ciagomem_ql::Stage::Take(_)
                                                | ciagomem_ql::Stage::SearchBm25 { .. }
                                        )
                                    }) {
                                        return Err(Error::Unsupported);
                                    }
                                    engine.execute(&query, &token).map(Output::Query)
                                }
                                Operation::Query(text) => engine
                                    .execute(
                                        &ciagomem_ql::parse(&text).map_err(|_| Error::Parse)?,
                                        &token,
                                    )
                                    .map(Output::Query),
                                Operation::Delete(actor) => {
                                    engine.delete(actor, &token).map(Output::Deleted)
                                }
                                Operation::Link {
                                    dependent,
                                    target,
                                    mode,
                                } => engine
                                    .add_link(dependent, target, mode, &token)
                                    .map(|_| Output::Unit),
                                Operation::BeginDelete(actor) => {
                                    engine.begin_delete(actor, &token).map(Output::DeleteJob)
                                }
                                Operation::AdvanceDelete { job, cursor, max } => engine
                                    .advance_delete(job, cursor, max, &token)
                                    .map(Output::DeleteJob),
                                Operation::Checkpoint => engine.checkpoint().map(|_| Output::Unit),
                            }
                        });
                    let _ = request.reply.send(Reply {
                        result,
                        _reservation: request.reservation,
                    });
                }
            })?;
        match readiness.recv() {
            Ok(Ok(())) => {}
            other => {
                let _ = worker.join();
                return Err(std::io::Error::other(format!(
                    "host startup failed: {other:?}"
                )));
            }
        }
        Ok(Self {
            client,
            worker: Some(worker),
        })
    }
    pub fn client(&self) -> Client {
        self.client.clone()
    }
    /// Drain accepted work; call outside a reactor. Retained replies keep their reservations.
    pub fn shutdown(mut self) -> std::thread::Result<()> {
        self.stop();
        self.worker.take().unwrap().join()
    }
    fn stop(&self) {
        if let Ok(mut state) = self.client.state.lock() {
            state.accepting = false;
        }
        let _ = self.client.sender.send(Message::Stop);
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.stop();
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}
