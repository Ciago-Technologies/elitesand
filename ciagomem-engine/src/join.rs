//! Bounded fan-out state for read joins; transport is the existing typed SPSC mesh.
use crate::{ActorRef, Error, Result, Snapshot};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[derive(Debug, Clone)]
pub struct Request {
    pub ticket: u64,
    pub tenant: u128,
    pub target: ActorRef,
    pub deadline_ns: u64,
}
#[derive(Debug, Clone)]
pub struct Reply {
    pub ticket: u64,
    pub target: ActorRef,
    pub snapshot: Result<Snapshot>,
}
pub struct Fanout {
    limit: usize,
    next: u64,
    pending: BTreeMap<u64, Request>,
    seen: BTreeSet<ActorRef>,
}
impl Fanout {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            next: 1,
            pending: BTreeMap::new(),
            seen: BTreeSet::new(),
        }
    }
    pub fn begin(&mut self, tenant: u128, target: ActorRef, deadline_ns: u64) -> Result<Request> {
        if self.pending.len() >= self.limit {
            return Err(Error::Budget);
        }
        if self.seen.contains(&target) {
            return Err(Error::Duplicate);
        }
        let ticket = self.next;
        self.next = self.next.checked_add(1).ok_or(Error::Overflow)?;
        let r = Request {
            ticket,
            tenant,
            target,
            deadline_ns,
        };
        self.pending.insert(ticket, r.clone());
        self.seen.insert(target);
        Ok(r)
    }
    pub fn complete(&mut self, reply: Reply, now: u64) -> Result<Arc<crate::Record>> {
        let r = self
            .pending
            .get(&reply.ticket)
            .ok_or(Error::StaleReference)?;
        if reply.target != r.target {
            return Err(Error::StaleReference);
        }
        // A forged or misrouted response cannot consume the outstanding valid ticket.
        if let Ok(s) = &reply.snapshot {
            if s.record.actor != r.target
                || s.record.tenant != r.tenant
                || !s.record.label.readers().contains(&r.tenant)
            {
                return Err(Error::ForeignActor);
            }
        }
        let r = self.pending.remove(&reply.ticket).unwrap();
        self.seen.remove(&r.target);
        if now >= r.deadline_ns {
            return Err(Error::Budget);
        }
        Ok(reply.snapshot?.record)
    }
    pub fn expire(&mut self, now: u64) -> usize {
        let tickets: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, r)| now >= r.deadline_ns)
            .map(|(t, _)| *t)
            .collect();
        for t in &tickets {
            let r = self.pending.remove(t).unwrap();
            self.seen.remove(&r.target);
        }
        tickets.len()
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug, Clone)]
pub struct Joined {
    pub left: Snapshot,
    pub right: Snapshot,
    pub label: ciagomem_core::Label,
}
/// Bounded read-only equijoin for relationships without direct ActorRef fields.
/// Keys use explicit Scalar types, like grouping; this API does not promise SQL coercions.
pub fn equi_join(
    left: &[Snapshot],
    right: &[Snapshot],
    left_field: &str,
    right_field: &str,
    session: &ciagomem_core::VerifiedMacaroon,
    limits: &crate::Limits,
) -> Result<Vec<Joined>> {
    use crate::Key;
    use std::collections::BTreeMap;
    if !session.allows(ciagomem_core::OP_READ) {
        return Err(Error::Permission);
    }
    if left.len().saturating_add(right.len()) > limits.query_work {
        return Err(Error::Budget);
    }
    // Authorize every candidate before constructing any relationship or returning output.
    for s in left.iter().chain(right) {
        if s.record.tenant != session.tenant_incarnation()
            || !s
                .record
                .label
                .readers()
                .contains(&session.tenant_incarnation())
        {
            return Err(Error::ForeignActor);
        }
    }
    let mut build: BTreeMap<Key, Vec<&Snapshot>> = BTreeMap::new();
    let mut scratch = 0usize;
    for row in right {
        if let Some(value) = row.record.fields.get(right_field) {
            if matches!(value, crate::Scalar::Null) {
                continue;
            }
            scratch = scratch.saturating_add(value.bytes() + 64);
            if scratch > limits.result_bytes {
                return Err(Error::Budget);
            }
            build.entry(value.key()?).or_default().push(row);
        }
    }
    let mut out = Vec::new();
    for a in left {
        let Some(value) = a.record.fields.get(left_field) else {
            continue;
        };
        if matches!(value, crate::Scalar::Null) {
            continue;
        }
        if let Some(matches) = build.get(&value.key()?) {
            for b in matches {
                if out.len() >= limits.join_rows
                    || out.len().saturating_add(1).saturating_mul(128) > limits.result_bytes
                {
                    return Err(Error::Budget);
                }
                out.push(Joined {
                    left: a.clone(),
                    right: (*b).clone(),
                    label: a.record.label.join(&b.record.label),
                });
            }
        }
    }
    Ok(out)
}
