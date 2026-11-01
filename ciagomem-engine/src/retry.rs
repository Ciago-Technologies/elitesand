//! Durable ordered client intents. Receipts are committed in the same batch as their effects.
use crate::{persistence::DurableJournal, *};
use ciagomem_core::{OP_READ, OP_WRITE, VerifiedMacaroon, content_digest};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequestKey {
    pub tenant: u128,
    pub client: u64,
    pub sequence: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub key: RequestKey,
    pub digest: [u8; 32],
    pub status: u16,
    pub commit_sequence: u64,
    pub expires_at: u64,
    pub result: Vec<u8>,
}
#[derive(Clone)]
pub enum WriteCommand {
    Create(Record),
    Replace {
        record: Record,
        expected_version: u64,
    },
    Mutate {
        actor: ActorRef,
        expected_version: u64,
        query: String,
    },
    Delete {
        actor: ActorRef,
        expected_version: u64,
    },
}
#[derive(Debug, Clone)]
pub struct LedgerLimits {
    pub clients: usize,
    pub per_tenant_clients: usize,
    pub receipts: usize,
    pub per_tenant_receipts: usize,
    pub per_tenant_bytes: usize,
    pub bytes: usize,
    pub retention_ns: u64,
}
impl Default for LedgerLimits {
    fn default() -> Self {
        Self {
            clients: 4096,
            per_tenant_clients: 1024,
            receipts: 16384,
            per_tenant_receipts: 4096,
            per_tenant_bytes: 4 << 20,
            bytes: 16 << 20,
            retention_ns: 86_400_000_000_000,
        }
    }
}
#[derive(Default)]
pub(crate) struct Ledger {
    pub clients: BTreeMap<(u128, u64), u64>,
    pub receipts: BTreeMap<RequestKey, Receipt>,
    pub limits: LedgerLimits,
    pub now: u64,
    expiry: BTreeSet<(u64, RequestKey)>,
    bytes: usize,
    tenants: BTreeMap<u128, (usize, usize)>,
}
impl Ledger {
    pub fn expire(&mut self, now: u64) {
        self.now = self.now.max(now);
        while self
            .expiry
            .first()
            .is_some_and(|(until, _)| *until <= self.now)
        {
            let (_, key) = self.expiry.pop_first().unwrap();
            if let Some(r) = self.receipts.remove(&key) {
                let bytes = 128 + r.result.len();
                self.bytes -= bytes;
                let tenant = self.tenants.get_mut(&key.tenant).unwrap();
                tenant.0 -= 1;
                tenant.1 -= bytes;
                if tenant.0 == 0 {
                    self.tenants.remove(&key.tenant);
                }
            }
        }
    }
    fn capacity(&self, tenant: u128, bytes: usize) -> Result<()> {
        let (count, used) = self.tenants.get(&tenant).copied().unwrap_or_default();
        if self.receipts.len() >= self.limits.receipts
            || self.bytes.saturating_add(bytes) > self.limits.bytes
            || count >= self.limits.per_tenant_receipts
            || used.saturating_add(bytes) > self.limits.per_tenant_bytes
        {
            Err(Error::DedupCapacity)
        } else {
            Ok(())
        }
    }
    pub fn apply(&mut self, r: Receipt, checkpoint: bool) -> Result<()> {
        if r.key.client == 0 || r.key.sequence == 0 || r.result.len() > 65536 {
            return Err(Error::InvalidImage);
        }
        let old = self
            .clients
            .get(&(r.key.tenant, r.key.client))
            .copied()
            .unwrap_or(0);
        if !checkpoint && old.checked_add(1) != Some(r.key.sequence) {
            return Err(Error::InvalidImage);
        }
        if let Some(existing) = self.receipts.get(&r.key) {
            if existing != &r {
                return Err(Error::InvalidImage);
            }
            return Ok(());
        }
        self.clients
            .insert((r.key.tenant, r.key.client), old.max(r.key.sequence));
        if r.expires_at > self.now {
            let bytes = 128 + r.result.len();
            self.capacity(r.key.tenant, bytes)?;
            self.bytes += bytes;
            let tenant = self.tenants.entry(r.key.tenant).or_default();
            tenant.0 += 1;
            tenant.1 += bytes;
            self.expiry.insert((r.expires_at, r.key));
            self.receipts.insert(r.key, r);
        }
        Ok(())
    }
    pub fn admit(
        &mut self,
        key: RequestKey,
        digest: [u8; 32],
        now: u64,
    ) -> Result<Option<Receipt>> {
        self.expire(now);
        if let Some(r) = self.receipts.get(&key) {
            return if r.digest == digest {
                Ok(Some(r.clone()))
            } else {
                Err(Error::IdempotencyConflict)
            };
        }
        let old = self
            .clients
            .get(&(key.tenant, key.client))
            .copied()
            .unwrap_or(0);
        if key.sequence <= old {
            return Err(Error::RetryTooOld);
        }
        if old.checked_add(1) != Some(key.sequence) {
            return Err(Error::SequenceGap);
        }
        if !self.clients.contains_key(&(key.tenant, key.client))
            && (self.clients.len() >= self.limits.clients
                || self
                    .clients
                    .range((key.tenant, 0)..=(key.tenant, u64::MAX))
                    .count()
                    >= self.limits.per_tenant_clients)
        {
            return Err(Error::DedupCapacity);
        }
        self.capacity(key.tenant, 512)?;
        Ok(None)
    }
}
pub fn command_digest(command: &WriteCommand) -> [u8; 32] {
    let mut bytes = b"ciagomem/write-command/v1".to_vec();
    match command {
        WriteCommand::Create(r) => {
            bytes.push(1);
            bytes.extend(image::encode(r));
        }
        WriteCommand::Replace {
            record,
            expected_version,
        } => {
            bytes.push(2);
            bytes.extend(expected_version.to_le_bytes());
            bytes.extend(image::encode(record));
        }
        WriteCommand::Mutate {
            actor,
            expected_version,
            query,
        } => {
            bytes.push(3);
            bytes.extend(actor.id.to_le_bytes());
            bytes.extend(actor.incarnation.to_le_bytes());
            bytes.extend(expected_version.to_le_bytes());
            bytes.extend(query.as_bytes());
        }
        WriteCommand::Delete {
            actor,
            expected_version,
        } => {
            bytes.push(4);
            bytes.extend(actor.id.to_le_bytes());
            bytes.extend(actor.incarnation.to_le_bytes());
            bytes.extend(expected_version.to_le_bytes());
        }
    };
    content_digest(&bytes)
}
pub fn status(error: &Error) -> u16 {
    match error {
        Error::AlreadyExists => 1,
        Error::MissingActor => 2,
        Error::Permission | Error::ForeignActor => 3,
        Error::MissingSchema => 4,
        Error::StaleReference => 5,
        Error::Budget => 8,
        Error::Duplicate | Error::IdempotencyConflict => 9,
        Error::RetryTooOld => 10,
        Error::SequenceGap => 11,
        Error::InvalidImage => 12,
        Error::Journal => 13,
        Error::Unsupported => 14,
        Error::DedupCapacity => 15,
        _ => 6,
    }
}
impl Engine<DurableJournal> {
    pub fn execute_retry(
        &mut self,
        key: RequestKey,
        command: WriteCommand,
        token: &VerifiedMacaroon,
    ) -> Result<Receipt> {
        self.authorize(token)?;
        if key.tenant != token.tenant_incarnation()
            || token.client_id() != Some(key.client)
            || !token.allows(OP_READ | OP_WRITE)
        {
            return Err(Error::Permission);
        }
        let digest = command_digest(&command);
        let now = self.observed_now();
        if let Some(receipt) = self.journal.ledger.admit(key, digest, now)? {
            return Ok(receipt);
        }
        let mut receipt = Receipt {
            key,
            digest,
            status: 0,
            commit_sequence: self.sequence.checked_add(1).ok_or(Error::Overflow)?,
            expires_at: now
                .checked_add(self.journal.ledger.limits.retention_ns)
                .ok_or(Error::Overflow)?,
            result: Vec::new(),
        };
        // Preflight actor identity/CAS and restrict network mutation to one actor and no response projection.
        let prepared = (|| -> Result<WriteCommand> {
            match command {
                WriteCommand::Create(mut record) => {
                    let mut fresh = None;
                    for _ in 0..16 {
                        let id = ciagomem_core::random_id().map_err(|_| Error::Journal)? as u64;
                        if id != 0 && !self.contains_actor_id(id) {
                            fresh = Some(id);
                            break;
                        }
                    }
                    record.actor.id = fresh.ok_or(Error::Journal)?;
                    record.actor.incarnation =
                        ciagomem_core::random_id().map_err(|_| Error::Journal)?;
                    receipt.result.extend(record.actor.id.to_le_bytes());
                    receipt
                        .result
                        .extend(record.actor.incarnation.to_le_bytes());
                    Ok(WriteCommand::Create(record))
                }
                WriteCommand::Replace {
                    record,
                    expected_version,
                } => {
                    self.check_version(record.actor, expected_version, token)?;
                    Ok(WriteCommand::Replace {
                        record,
                        expected_version,
                    })
                }
                WriteCommand::Delete {
                    actor,
                    expected_version,
                } => {
                    self.check_version(actor, expected_version, token)?;
                    Ok(WriteCommand::Delete {
                        actor,
                        expected_version,
                    })
                }
                WriteCommand::Mutate {
                    actor,
                    expected_version,
                    query,
                } => {
                    self.check_version(actor, expected_version, token)?;
                    let parsed = ciagomem_ql::parse(&query).map_err(|_| Error::Parse)?;
                    if !matches!(parsed.source,ciagomem_ql::Source::Actor(id) if id==actor.id)
                        || !matches!(parsed.stages.last(), Some(ciagomem_ql::Stage::Mutate(_)))
                        || parsed
                            .stages
                            .iter()
                            .take(parsed.stages.len().saturating_sub(1))
                            .any(|s| !matches!(s, ciagomem_ql::Stage::Filter(_)))
                    {
                        return Err(Error::Unsupported);
                    }
                    drop(parsed);
                    Ok(WriteCommand::Mutate {
                        actor,
                        expected_version,
                        query,
                    })
                }
            }
        })();
        if let Err(error) = &prepared {
            receipt.status = status(error);
            receipt.result.clear();
        }
        self.journal.pending = Some(receipt.clone());
        let result = match prepared {
            Err(error) => Err(error),
            Ok(WriteCommand::Create(r) | WriteCommand::Replace { record: r, .. }) => {
                self.put(r, token)
            }
            Ok(WriteCommand::Delete { actor, .. }) => self.delete(actor, token).map(|_| ()),
            Ok(WriteCommand::Mutate { query, .. }) => self
                .execute(
                    &ciagomem_ql::parse(&query).map_err(|_| Error::Parse)?,
                    token,
                )
                .and_then(|r| {
                    if r.rows.is_empty() {
                        Err(Error::ConditionFailed)
                    } else {
                        Ok(())
                    }
                }),
        };
        if matches!(
            result,
            Err(Error::Budget | Error::Journal | Error::Permission | Error::ForeignActor)
        ) {
            self.journal.pending = None;
            return result.map(|_| receipt);
        }
        if let Some(mut pending) = self.journal.pending.take() {
            if let Err(error) = result {
                pending.status = status(&error);
                pending.result.clear();
            }
            self.journal.pending = Some(pending);
            if let Err(error) = self.commit(Vec::new()) {
                self.journal.pending = None;
                return Err(error);
            }
        }
        self.journal
            .ledger
            .receipts
            .get(&key)
            .cloned()
            .ok_or(Error::Journal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receipt(tenant: u128, client: u64, sequence: u64, expires_at: u64) -> Receipt {
        Receipt {
            key: RequestKey {
                tenant,
                client,
                sequence,
            },
            digest: [1; 32],
            status: 0,
            commit_sequence: sequence,
            expires_at,
            result: vec![],
        }
    }
    #[test]
    fn expired_receipt_keeps_high_water_and_clock_cannot_revive_it() {
        let mut ledger = Ledger::default();
        let r = receipt(1, 2, 1, 10);
        ledger.apply(r.clone(), false).unwrap();
        assert_eq!(ledger.admit(r.key, r.digest, 11), Err(Error::RetryTooOld));
        assert_eq!(ledger.admit(r.key, r.digest, 1), Err(Error::RetryTooOld));
        assert_eq!(ledger.bytes, 0);
        assert!(ledger.tenants.is_empty());
        assert_eq!(
            ledger.admit(
                RequestKey {
                    sequence: 2,
                    ..r.key
                },
                r.digest,
                1
            ),
            Ok(None)
        );
    }
    #[test]
    fn one_tenant_cannot_fill_every_receipt_credit() {
        let mut ledger = Ledger::default();
        ledger.limits.per_tenant_receipts = 1;
        let r = receipt(1, 2, 1, 10);
        ledger.apply(r.clone(), false).unwrap();
        assert_eq!(
            ledger.admit(
                RequestKey {
                    sequence: 2,
                    ..r.key
                },
                r.digest,
                1
            ),
            Err(Error::DedupCapacity)
        );
        assert_eq!(
            ledger.admit(RequestKey { tenant: 2, ..r.key }, r.digest, 1),
            Ok(None)
        );
    }
}
