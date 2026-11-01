//! Authenticated application batches over the existing group-commit WAL.
//! File I/O runs on a single storage worker; this is not the target pinned reactor host.
use crate::{
    actor_generated::ciagomem::image as fb,
    links::{Edge, OnDelete},
    *,
};
use ciagomem_core::{KeyedPrf, Label, Prf, ct_eq};
use ciagomem_wal::{driver::FileLog, gclog::LogConfig};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub struct DurableJournal {
    log: FileLog,
    key: KeyedPrf,
    max_batch: usize,
    directory: PathBuf,
    max_recovery: usize,
    pub(crate) ledger: crate::retry::Ledger,
    pub(crate) pending: Option<crate::retry::Receipt>,
}
impl Journal for DurableJournal {
    fn commit(&mut self, batch: &Batch) -> Result<()> {
        let mut committed = batch.clone();
        if let Some(receipt) = &self.pending {
            committed.changes.push(Change::Receipt(receipt.clone()));
        }
        let payload = encode_batch(&committed, &self.key, self.max_batch)?;
        self.log.append(&payload).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Error::Budget
            } else {
                Error::Journal
            }
        })?;
        if let Some(receipt) = self.pending.take() {
            self.ledger.apply(receipt, false)?;
        }
        Ok(())
    }
}
impl DurableJournal {
    /// Call on the eventual storage-owner thread before accepting work.
    #[cfg(target_os = "linux")]
    pub fn enable_uring(&mut self) -> std::io::Result<()> {
        self.log.enable_uring()
    }
    pub fn durable_lsn(&self) -> u64 {
        self.log.durable_lsn()
    }
}
fn invalid() -> Error {
    Error::InvalidImage
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            return Err(invalid());
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<usize> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn actor(&mut self) -> Result<ActorRef> {
        let id = self.u64()?;
        let incarnation = u128::from_le_bytes(self.take(16)?.try_into().unwrap());
        if id == 0 || incarnation == 0 {
            return Err(invalid());
        }
        Ok(ActorRef { id, incarnation })
    }
}
fn put_actor(out: &mut Vec<u8>, actor: ActorRef) {
    out.extend(actor.id.to_le_bytes());
    out.extend(actor.incarnation.to_le_bytes());
}
fn sized(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    let n = u32::try_from(bytes.len()).map_err(|_| Error::Budget)?;
    out.extend(n.to_le_bytes());
    out.extend(bytes);
    Ok(())
}
fn encode_batch(batch: &Batch, key: &KeyedPrf, max: usize) -> Result<Vec<u8>> {
    let mut out = b"CMEB0002".to_vec();
    out.extend(batch.sequence.to_le_bytes());
    out.extend(
        u32::try_from(batch.changes.len())
            .map_err(|_| Error::Budget)?
            .to_le_bytes(),
    );
    for change in &batch.changes {
        match change {
            Change::Put(s) => {
                out.push(1);
                sized(&mut out, &s.image)?;
                sized(&mut out, &s.record.label.seal_checkpoint(key, &s.image))?;
            }
            Change::Delete(a) => {
                out.push(2);
                put_actor(&mut out, *a);
            }
            Change::Link(e) => {
                out.push(3);
                put_actor(&mut out, e.dependent);
                put_actor(&mut out, e.target);
                out.push(if e.on_delete == OnDelete::Restrict {
                    0
                } else {
                    1
                });
            }
            Change::BeginDelete { job, plan } => {
                out.push(4);
                out.extend(job.to_le_bytes());
                out.extend(
                    u32::try_from(plan.len())
                        .map_err(|_| Error::Budget)?
                        .to_le_bytes(),
                );
                for a in plan {
                    put_actor(&mut out, *a);
                }
            }
            Change::DeleteProgress { job, cursor } => {
                out.push(5);
                out.extend(job.to_le_bytes());
                out.extend(cursor.to_le_bytes());
            }
            Change::Receipt(r) => {
                out.push(6);
                out.extend(r.key.tenant.to_le_bytes());
                out.extend(r.key.client.to_le_bytes());
                out.extend(r.key.sequence.to_le_bytes());
                out.extend(r.digest);
                out.extend(r.status.to_le_bytes());
                out.extend(r.commit_sequence.to_le_bytes());
                out.extend(r.expires_at.to_le_bytes());
                sized(&mut out, &r.result)?;
            }
            Change::ClientHighWater {
                tenant,
                client,
                sequence,
            } => {
                out.push(7);
                out.extend(tenant.to_le_bytes());
                out.extend(client.to_le_bytes());
                out.extend(sequence.to_le_bytes());
            }
            Change::DocumentVersion { actor, version } => {
                out.push(8);
                put_actor(&mut out, *actor);
                out.extend(version.to_le_bytes());
            }
        }
        if out.len().saturating_add(32) > max {
            return Err(Error::Budget);
        }
    }
    let tag = key.mac256(b"ciagomem/engine-batch/v1", &out);
    out.extend(tag);
    Ok(out)
}
fn decode_batch(bytes: &[u8], key: &KeyedPrf, limits: &Limits, max: usize) -> Result<Batch> {
    if bytes.len() < 52 || bytes.len() > max {
        return Err(invalid());
    }
    let (body, tag) = bytes.split_at(bytes.len() - 32);
    if !ct_eq(tag, &key.mac256(b"ciagomem/engine-batch/v1", body)) {
        return Err(invalid());
    }
    let mut input = Input(body);
    if !matches!(input.take(8)?, b"CMEB0001" | b"CMEB0002") {
        return Err(invalid());
    }
    let sequence = input.u64()?;
    let count = input.u32()?;
    if count > body.len() / 9 {
        return Err(invalid());
    }
    let mut changes = Vec::new();
    for _ in 0..count {
        changes.push(match input.take(1)?[0] {
            1 => {
                let len = input.u32()?;
                let image = input.take(len)?;
                let len = input.u32()?;
                let sealed = input.take(len)?;
                let label = Label::open_checkpoint(key, image, sealed).map_err(|_| invalid())?;
                let record = decode_record(image, label, limits)?;
                Change::Put(Snapshot {
                    record: std::sync::Arc::new(record),
                    image: image.to_vec().into(),
                })
            }
            2 => Change::Delete(input.actor()?),
            3 => Change::Link(Edge {
                dependent: input.actor()?,
                target: input.actor()?,
                on_delete: match input.take(1)?[0] {
                    0 => OnDelete::Restrict,
                    1 => OnDelete::Cascade,
                    _ => return Err(invalid()),
                },
            }),
            4 => {
                let job = input.u64()?;
                let len = input.u32()?;
                if len > limits.cascade_rows || len > input.0.len() / 24 {
                    return Err(invalid());
                }
                let mut plan = Vec::with_capacity(len);
                for _ in 0..len {
                    plan.push(input.actor()?);
                }
                Change::BeginDelete { job, plan }
            }
            5 => Change::DeleteProgress {
                job: input.u64()?,
                cursor: input.u64()?,
            },
            6 => {
                let tenant = u128::from_le_bytes(input.take(16)?.try_into().unwrap());
                let client = input.u64()?;
                let seq = input.u64()?;
                let digest = input.take(32)?.try_into().unwrap();
                let status = u16::from_le_bytes(input.take(2)?.try_into().unwrap());
                let commit_sequence = input.u64()?;
                let expires_at = input.u64()?;
                let len = input.u32()?;
                if len > 65536 || status > 16 || commit_sequence == 0 || commit_sequence > sequence
                {
                    return Err(invalid());
                }
                Change::Receipt(crate::retry::Receipt {
                    key: crate::retry::RequestKey {
                        tenant,
                        client,
                        sequence: seq,
                    },
                    digest,
                    status,
                    commit_sequence,
                    expires_at,
                    result: input.take(len)?.to_vec(),
                })
            }
            7 => Change::ClientHighWater {
                tenant: u128::from_le_bytes(input.take(16)?.try_into().unwrap()),
                client: input.u64()?,
                sequence: input.u64()?,
            },
            8 => Change::DocumentVersion {
                actor: input.actor()?,
                version: input.u64()?,
            },
            _ => return Err(invalid()),
        });
    }
    if !input.0.is_empty() {
        return Err(invalid());
    }
    Ok(Batch { sequence, changes })
}
pub(crate) fn decode_record(bytes: &[u8], label: Label, limits: &Limits) -> Result<Record> {
    let view = image::view(bytes)?;
    if view.fields().len() > limits.fields
        || view
            .embedding()
            .is_some_and(|v| v.len() > limits.vector_dims)
    {
        return Err(Error::Budget);
    }
    let readers: Vec<_> = view
        .reader_lo()
        .iter()
        .zip(view.reader_hi().iter())
        .map(|(lo, hi)| lo as u128 | ((hi as u128) << 64))
        .collect();
    if view.integrity() != label.integrity() as u8
        || view.sources() != label.sources()
        || readers != label.readers()
    {
        return Err(invalid());
    }
    let mut fields = Fields::new();
    for f in view.fields() {
        let value = match f.kind() {
            fb::Kind::Null => Scalar::Null,
            fb::Kind::Bool => Scalar::Bool(f.bool_value()),
            fb::Kind::Int => Scalar::Int(f.int_value()),
            fb::Kind::Float => Scalar::Float(f.float_value()),
            fb::Kind::Text => Scalar::Text(f.text_value().ok_or_else(invalid)?.into()),
            fb::Kind::Ref => Scalar::Ref(ActorRef {
                id: f.ref_id(),
                incarnation: f.ref_inc_lo() as u128 | ((f.ref_inc_hi() as u128) << 64),
            }),
            _ => return Err(invalid()),
        };
        if fields.insert(f.name().into(), value).is_some() {
            return Err(invalid());
        }
    }
    let record = Record {
        actor: ActorRef {
            id: view.id(),
            incarnation: view.incarnation_lo() as u128 | ((view.incarnation_hi() as u128) << 64),
        },
        tenant: view.tenant_lo() as u128 | ((view.tenant_hi() as u128) << 64),
        schema_version: view.schema_version(),
        fields,
        label,
        embedding: view
            .embedding()
            .map(|v| v.iter().collect())
            .unwrap_or_default(),
    };
    limits.validate(&record)?;
    Ok(record)
}
impl Engine<DurableJournal> {
    /// Open a trusted private directory. Schema code must be installed before replay.
    pub fn open_durable(
        directory: &Path,
        cfg: LogConfig,
        max_recovery_bytes: usize,
        cores: usize,
        limits: Limits,
        schemas: schema::Schemas,
        key: KeyedPrf,
        boot: u128,
        clock: std::sync::Arc<dyn ciagomem_core::Clock + Send + Sync>,
    ) -> Result<Self> {
        let (log, payloads) = FileLog::open(&directory.join("engine.wal"), cfg, max_recovery_bytes)
            .map_err(|_| Error::Journal)?;
        let max_batch = cfg.buf_bytes.saturating_sub(2 * 32 + cfg.block + 32);
        let journal = DurableJournal {
            log,
            key,
            max_batch,
            directory: directory.into(),
            max_recovery: max_recovery_bytes,
            ledger: crate::retry::Ledger::default(),
            pending: None,
        };
        let mut engine = Engine::new(cores, limits, journal, boot, clock)?;
        engine.schemas = schemas;
        engine.journal.ledger.now = engine.observed_now();
        let mut checkpoint = None;
        let mut present = false;
        for index in 0..2 {
            let path = directory.join(format!("checkpoint.{index}"));
            let mut f = match File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::Journal),
            };
            present = true;
            if f.metadata().map_err(|_| Error::Journal)?.len() > max_recovery_bytes as u64 {
                continue;
            }
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).map_err(|_| Error::Journal)?;
            if bytes.len() < 16 || &bytes[..8] != b"CMCP0001" {
                continue;
            }
            let lsn = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            // Authenticate the LSN as well as the checkpoint body.
            if bytes.len() < 48 {
                continue;
            }
            let (body, tag) = bytes.split_at(bytes.len() - 32);
            if !ct_eq(
                tag,
                &engine.journal.key.mac256(b"ciagomem/checkpoint/v1", body),
            ) {
                continue;
            }
            if let Ok(batch) = decode_batch(
                &bytes[16..bytes.len() - 32],
                &engine.journal.key,
                &engine.limits,
                max_recovery_bytes,
            ) {
                if checkpoint
                    .as_ref()
                    .is_none_or(|(old, _): &(u64, Batch)| lsn > *old)
                {
                    checkpoint = Some((lsn, batch));
                }
            }
        }
        if present && checkpoint.is_none() {
            return Err(invalid());
        }
        if let Some((lsn, batch)) = checkpoint {
            if lsn > engine.journal.log.durable_lsn()
                || engine.journal.log.recovery_start() > lsn.saturating_add(1)
            {
                return Err(invalid());
            }
            engine.restore_ledger(&batch.changes, true)?;
            engine.restore_checkpoint(batch)?;
        } else if engine.journal.log.recovery_start() != 1 {
            return Err(invalid());
        }
        for payload in payloads {
            let batch = decode_batch(&payload, &engine.journal.key, &engine.limits, max_batch)?;
            if batch.sequence > engine.sequence {
                engine.restore_ledger(&batch.changes, false)?;
                engine.replay_batch(batch)?;
            }
        }
        match std::fs::remove_file(directory.join("checkpoint.pending")) {
            Ok(()) => File::open(directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| Error::Journal)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Journal),
        }
        Ok(engine)
    }
    fn restore_ledger(&mut self, changes: &[Change], checkpoint: bool) -> Result<()> {
        for change in changes {
            match change {
                Change::ClientHighWater {
                    tenant,
                    client,
                    sequence,
                } => {
                    if !checkpoint || *client == 0 || *sequence == 0 {
                        return Err(invalid());
                    }
                    self.journal
                        .ledger
                        .clients
                        .insert((*tenant, *client), *sequence);
                }
                Change::Receipt(r) => {
                    self.journal.ledger.apply(r.clone(), checkpoint)?;
                }
                _ => {}
            }
        }
        let now = self.journal.ledger.now;
        self.journal.ledger.expire(now);
        if self.journal.ledger.clients.len() > self.journal.ledger.limits.clients
            || self.journal.ledger.receipts.len() > self.journal.ledger.limits.receipts
        {
            return Err(Error::DedupCapacity);
        }
        Ok(())
    }
    /// Native mode must be enabled on the eventual owner thread, never silently emulated.
    #[cfg(target_os = "linux")]
    pub fn enable_uring(&mut self) -> std::io::Result<()> {
        self.journal.enable_uring()
    }
    /// Atomic full-state checkpoint; WAL reclamation follows file and directory persistence.
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.journal.log.is_fenced() {
            return Err(Error::Journal);
        }
        let mut batch = self.checkpoint_batch();
        for (&(tenant, client), &sequence) in &self.journal.ledger.clients {
            batch.changes.push(Change::ClientHighWater {
                tenant,
                client,
                sequence,
            });
        }
        for receipt in self.journal.ledger.receipts.values() {
            batch.changes.push(Change::Receipt(receipt.clone()));
        }
        let payload = encode_batch(&batch, &self.journal.key, self.journal.max_recovery)?;
        let lsn = self.journal.log.durable_lsn();
        let mut bytes = b"CMCP0001".to_vec();
        bytes.extend(lsn.to_le_bytes());
        bytes.extend(payload);
        let tag = self.journal.key.mac256(b"ciagomem/checkpoint/v1", &bytes);
        bytes.extend(tag);
        if bytes.len() > self.journal.max_recovery {
            return Err(Error::Budget);
        }
        let target = self
            .journal
            .directory
            .join(format!("checkpoint.{}", self.sequence % 2));
        let temporary = self.journal.directory.join("checkpoint.pending");
        // create_new prevents following/replacing an unexpected existing staging file.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| Error::Journal)?;
        let written = file.write_all(&bytes).and_then(|_| file.sync_all());
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(Error::Journal);
        }
        std::fs::rename(&temporary, &target).map_err(|_| Error::Journal)?;
        File::open(&self.journal.directory)
            .and_then(|f| f.sync_all())
            .map_err(|_| Error::Journal)?;
        self.journal
            .log
            .checkpointed(lsn)
            .map_err(|_| Error::Journal)
    }
}
