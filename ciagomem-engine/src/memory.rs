//! Bounded in-process worker handoffs. No model runner, filesystem or durable task service.
use crate::{ActorRef, Engine, Error, Journal, Record, Result, Scalar, Snapshot};
use ciagomem_core::{Label, OP_READ, OP_WRITE, VerifiedMacaroon, content_digest, random_id};
use std::{collections::BTreeMap, fmt::Write};

#[derive(Debug, Clone)]
pub struct MemoryLimits {
    pub tasks: usize,
    pub sources: usize,
    pub source_bytes: usize,
    pub source_reads: u16,
    pub summary_bytes: usize,
    pub context_bytes: usize,
    pub context_items: usize,
    pub lease_ns: u64,
    pub summary_ns: u64,
}
impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            tasks: 64,
            sources: 16,
            source_bytes: 1 << 20,
            source_reads: 1,
            summary_bytes: 4096,
            context_bytes: 8192,
            context_items: 8,
            lease_ns: 300_000_000_000,
            summary_ns: 900_000_000_000,
        }
    }
}
/// Opaque local authority; the verified worker session is also required at every call.
/// ```compile_fail
/// use ciagomem_engine::memory::WorkerLease;
/// let forged = WorkerLease { broker: 1, job: 1 };
/// ```
#[derive(Debug)]
pub struct WorkerLease {
    broker: u128,
    job: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRef {
    broker: u128,
    job: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStamp {
    pub actor: ActorRef,
    pub digest: [u8; 32],
}
#[derive(Debug)]
pub struct ContextSlice {
    /// Untrusted evidence text, never a policy or tool instruction.
    pub text: String,
    pub label: Label,
    pub citations: Vec<SourceStamp>,
    pub omitted: usize,
}
impl ContextSlice {
    /// Unsigned model output enters a Var/data frame with explicit External integrity.
    /// Preserve its source categories and reader intersection; never mint a Fact permit.
    pub fn evidence_label(&self) -> Label {
        Label::external(self.label.sources(), self.label.readers().to_vec())
            .expect("joined broker labels have at most four readers")
    }
}
struct Task {
    output: ActorRef,
    worker: u128,
    deadline: u64,
    reads: u16,
    stale: bool,
    sources: Vec<SourceStamp>,
    published: Option<([u8; 32], u64)>,
}
/// Owns its Engine so workers cannot bypass lease/output validation via raw put/query.
/// One broker is bound to one tenant, audience and controller session. The host must
/// authenticate sessions with its trusted issuer and separately bound broker instances
/// and caller-held snapshots. This is not isolation from arbitrary native plugins that
/// can issue/verify Macaroons with their own keys; models receive only scoped wrappers.
pub struct MemoryBroker<J: Journal> {
    engine: Engine<J>,
    tenant: u128,
    controller: u128,
    audience: u64,
    id: u128,
    next: u64,
    limits: MemoryLimits,
    tasks: BTreeMap<u64, Task>,
}
impl<J: Journal> MemoryBroker<J> {
    pub fn new(
        engine: Engine<J>,
        controller: &VerifiedMacaroon,
        limits: MemoryLimits,
    ) -> Result<Self> {
        engine.authorize(controller)?;
        if !controller.allows(OP_READ | OP_WRITE) {
            return Err(Error::Permission);
        }
        if limits.tasks == 0
            || limits.tasks > 1024
            || limits.sources == 0
            || limits.sources > 32
            || limits.source_bytes == 0
            || limits.source_bytes > 16 << 20
            || limits.source_reads == 0
            || limits.source_reads > 16
            || limits.summary_bytes == 0
            || limits.summary_bytes > 16384
            || limits.context_bytes == 0
            || limits.context_bytes > 65536
            || limits.context_items == 0
            || limits.context_items > 64
            || limits.lease_ns == 0
            || limits.lease_ns > 300_000_000_000
            || limits.summary_ns == 0
            || limits.summary_ns > 3_600_000_000_000
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self {
            engine,
            tenant: controller.tenant_incarnation(),
            controller: controller.session_id(),
            audience: controller.audience(),
            id: random_id().map_err(|_| Error::Permission)?,
            next: 1,
            limits,
            tasks: BTreeMap::new(),
        })
    }
    fn session(&self, token: &VerifiedMacaroon, op: u64) -> Result<()> {
        self.engine.authorize(token)?;
        if token.tenant_incarnation() != self.tenant
            || token.audience() != self.audience
            || !token.allows(op)
        {
            return Err(Error::Permission);
        }
        Ok(())
    }
    fn controller(&self, token: &VerifiedMacaroon, op: u64) -> Result<()> {
        self.session(token, op)?;
        if token.session_id() != self.controller {
            return Err(Error::Permission);
        }
        Ok(())
    }
    fn worker(&self, lease: &WorkerLease, token: &VerifiedMacaroon, op: u64) -> Result<&Task> {
        self.session(token, op)?;
        if lease.broker != self.id {
            return Err(Error::Permission);
        }
        let task = self.tasks.get(&lease.job).ok_or(Error::StaleReference)?;
        if token.session_id() != task.worker || self.engine.observed_now() >= task.deadline {
            return Err(Error::Permission);
        }
        Ok(task)
    }
    /// Trusted ingestion adapter: no arbitrary file/URL fetching is performed here.
    pub fn ingest(&mut self, source: Record, controller: &VerifiedMacaroon) -> Result<()> {
        self.controller(controller, OP_WRITE)?;
        if self.tasks.values().any(|t| t.output.id == source.actor.id) {
            return Err(Error::Restricted);
        }
        let actor = source.actor;
        self.engine.put(source, controller)?;
        self.invalidate(actor);
        Ok(())
    }
    fn invalidate(&mut self, actor: ActorRef) {
        let mut changed = vec![actor];
        while let Some(actor) = changed.pop() {
            for task in self.tasks.values_mut() {
                if !task.stale && task.sources.iter().any(|s| s.actor.id == actor.id) {
                    task.stale = true;
                    changed.push(task.output);
                }
            }
        }
    }
    fn source_expiry(&self, actor: ActorRef) -> Result<u64> {
        if let Some(task) = self.tasks.values().find(|t| t.output == actor) {
            let (_, until) = task.published.ok_or(Error::StaleReference)?;
            if task.stale || self.engine.observed_now() >= until {
                return Err(Error::StaleReference);
            }
            return Ok(until);
        }
        Ok(u64::MAX)
    }
    pub fn delegate(
        &mut self,
        output: ActorRef,
        sources: &[ActorRef],
        worker: &VerifiedMacaroon,
        controller: &VerifiedMacaroon,
    ) -> Result<WorkerLease> {
        self.controller(controller, OP_READ | OP_WRITE)?;
        self.session(worker, OP_READ | OP_WRITE)?;
        if worker.session_id() == self.controller {
            return Err(Error::Permission);
        }
        if self.tasks.len() >= self.limits.tasks || sources.len() > self.limits.sources {
            return Err(Error::Budget);
        }
        if sources.is_empty() || output.id == 0 || output.incarnation == 0 {
            return Err(Error::InvalidValue);
        }
        if self.tasks.values().any(|t| t.output.id == output.id) {
            return Err(Error::Duplicate);
        }
        match self.engine.snapshot(output, controller) {
            Err(Error::MissingActor) => (),
            Ok(_) => return Err(Error::Duplicate),
            Err(e) => return Err(e),
        }
        let mut stamps = Vec::with_capacity(sources.len());
        let mut bytes = 0usize;
        let mut parent_until = u64::MAX;
        for source in sources {
            if source.id == output.id
                || stamps.iter().any(|s: &SourceStamp| s.actor.id == source.id)
            {
                return Err(Error::Duplicate);
            }
            parent_until = parent_until.min(self.source_expiry(*source)?);
            let s = self.engine.snapshot(*source, controller)?;
            // Also verify the worker can read this exact source before issuing a lease.
            self.engine.snapshot(*source, worker)?;
            bytes = bytes.checked_add(s.image.len()).ok_or(Error::Budget)?;
            if bytes > self.limits.source_bytes {
                return Err(Error::Budget);
            }
            stamps.push(SourceStamp {
                actor: *source,
                digest: content_digest(&s.image),
            });
        }
        let deadline = self
            .engine
            .observed_now()
            .checked_add(self.limits.lease_ns)
            .ok_or(Error::Overflow)?
            .min(worker.expires_at())
            .min(controller.expires_at())
            .min(parent_until);
        if self.engine.observed_now() >= deadline {
            return Err(Error::Permission);
        }
        let job = self.next;
        self.next = job.checked_add(1).ok_or(Error::Overflow)?;
        self.tasks.insert(
            job,
            Task {
                output,
                worker: worker.session_id(),
                deadline,
                reads: 0,
                stale: false,
                sources: stamps,
                published: None,
            },
        );
        Ok(WorkerLease {
            broker: self.id,
            job,
        })
    }
    fn sources(&self, task: &Task, token: &VerifiedMacaroon) -> Result<Vec<Snapshot>> {
        if task.stale {
            return Err(Error::StaleReference);
        }
        let mut out = Vec::with_capacity(task.sources.len());
        let mut bytes = 0usize;
        for source in &task.sources {
            self.source_expiry(source.actor)?;
            let s = self.engine.snapshot(source.actor, token)?;
            bytes = bytes.checked_add(s.image.len()).ok_or(Error::Budget)?;
            if bytes > self.limits.source_bytes {
                return Err(Error::Budget);
            }
            out.push(s);
        }
        Ok(out)
    }
    pub fn read_sources(
        &mut self,
        lease: &WorkerLease,
        worker: &VerifiedMacaroon,
    ) -> Result<Vec<Snapshot>> {
        let task = self.worker(lease, worker, OP_READ)?;
        if task.published.is_some() || task.reads >= self.limits.source_reads {
            return Err(Error::Budget);
        }
        let sources = self.sources(task, worker)?;
        self.worker(lease, worker, OP_READ)?;
        self.tasks.get_mut(&lease.job).unwrap().reads += 1;
        Ok(sources)
    }
    /// Same-content retries of a live, non-stale publication do not write the journal again.
    pub fn publish(
        &mut self,
        lease: &WorkerLease,
        summary: &str,
        worker: &VerifiedMacaroon,
    ) -> Result<MemoryRef> {
        let task = self.worker(lease, worker, OP_WRITE)?;
        if task.stale {
            return Err(Error::StaleReference);
        }
        if summary.is_empty() || summary.len() > self.limits.summary_bytes {
            return Err(Error::Budget);
        }
        let digest = content_digest(summary.as_bytes());
        if let Some((previous, until)) = task.published {
            if self.engine.observed_now() >= until {
                return Err(Error::StaleReference);
            }
            if previous != digest {
                return Err(Error::Duplicate);
            }
            return Ok(MemoryRef {
                broker: self.id,
                job: lease.job,
            });
        }
        let sources = self.sources(task, worker)?;
        let parents: Vec<_> = sources.iter().map(|s| s.record.label.clone()).collect();
        let label = Label::derived(summary.as_bytes(), &parents);
        let mut fields = [("summary".into(), Scalar::Text(summary.into()))]
            .into_iter()
            .collect::<crate::Fields>();
        for (i, stamp) in task.sources.iter().enumerate() {
            fields.insert(format!("source_{i}"), Scalar::Ref(stamp.actor));
            fields.insert(format!("digest_{i}"), Scalar::Text(hex(&stamp.digest)));
        }
        let record = Record::new(task.output, self.tenant, fields, label);
        let mut until = self
            .engine
            .observed_now()
            .checked_add(self.limits.summary_ns)
            .ok_or(Error::Overflow)?;
        for source in &task.sources {
            until = until.min(self.source_expiry(source.actor)?);
        }

        self.worker(lease, worker, OP_WRITE)?;
        self.engine.put(record, worker)?;
        self.tasks.get_mut(&lease.job).unwrap().published = Some((digest, until));
        Ok(MemoryRef {
            broker: self.id,
            job: lease.job,
        })
    }
    /// Historical completion metadata after a lost message; read_context checks freshness.
    pub fn result(
        &self,
        lease: &WorkerLease,
        controller: &VerifiedMacaroon,
    ) -> Result<Option<MemoryRef>> {
        self.controller(controller, OP_READ)?;
        if lease.broker != self.id {
            return Err(Error::Permission);
        }
        let task = self.tasks.get(&lease.job).ok_or(Error::StaleReference)?;
        Ok(task.published.map(|_| MemoryRef {
            broker: self.id,
            job: lease.job,
        }))
    }
    /// Ordered whole-item packing. Citation and framing bytes count against the limit.
    pub fn read_context(
        &self,
        refs: &[MemoryRef],
        byte_budget: usize,
        controller: &VerifiedMacaroon,
    ) -> Result<ContextSlice> {
        self.controller(controller, OP_READ)?;
        if refs.is_empty() {
            return Err(Error::InvalidValue);
        }
        if refs.len() > self.limits.context_items
            || byte_budget == 0
            || byte_budget > self.limits.context_bytes
        {
            return Err(Error::Budget);
        }
        let mut text = String::new();
        let mut citations = Vec::new();
        let mut label: Option<Label> = None;
        let mut seen = Vec::new();
        let mut omitted = 0;
        for reference in refs {
            if reference.broker != self.id {
                return Err(Error::Permission);
            }
            if seen.contains(&reference.job) {
                continue;
            }
            seen.push(reference.job);
            let task = self
                .tasks
                .get(&reference.job)
                .ok_or(Error::StaleReference)?;
            let (digest, until) = task.published.ok_or(Error::MissingActor)?;
            if self.engine.observed_now() >= until {
                return Err(Error::StaleReference);
            }
            self.sources(task, controller)?;
            let snapshot = self.engine.snapshot(task.output, controller)?;
            let Some(Scalar::Text(summary)) = snapshot.record.fields.get("summary") else {
                return Err(Error::InvalidImage);
            };
            if content_digest(summary.as_bytes()) != digest {
                return Err(Error::InvalidImage);
            }
            let mut item = format!(
                "MEMORY {}:{:032x}\nSUMMARY {} bytes\n{}\n",
                task.output.id,
                task.output.incarnation,
                summary.len(),
                summary
            );
            for s in &task.sources {
                writeln!(
                    item,
                    "SOURCE {}:{:032x} sha256={}",
                    s.actor.id,
                    s.actor.incarnation,
                    hex(&s.digest)
                )
                .unwrap();
            }
            if item.len() > byte_budget - text.len() {
                omitted += 1;
                continue;
            }
            text.push_str(&item);
            for stamp in &task.sources {
                if !citations.contains(stamp) {
                    citations.push(stamp.clone());
                }
            }
            label = Some(label.map_or_else(
                || snapshot.record.label.clone(),
                |l| l.join(&snapshot.record.label),
            ));
        }
        self.controller(controller, OP_READ)?;
        Ok(ContextSlice {
            text,
            citations,
            omitted,
            label: label.ok_or(Error::Budget)?,
        })
    }
    /// Explicit bounded retention; delete publication before releasing its reservation.
    pub fn forget(&mut self, lease: &WorkerLease, controller: &VerifiedMacaroon) -> Result<()> {
        self.controller(controller, OP_WRITE)?;
        if lease.broker != self.id {
            return Err(Error::Permission);
        }
        let task = self.tasks.get(&lease.job).ok_or(Error::StaleReference)?;
        if task.published.is_some() {
            let output = task.output;
            self.engine.delete(output, controller)?;
            self.invalidate(output);
        }
        self.tasks.remove(&lease.job);
        Ok(())
    }
}
fn hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for byte in digest {
        write!(s, "{byte:02x}").unwrap();
    }
    s
}

#[cfg(test)]
mod invalidation_tests {
    use super::*;
    #[test]
    fn replacement_event_invalidates_old_incarnation_and_its_descendants() {
        let actor = |id, incarnation| ActorRef { id, incarnation };
        let task = |output, source| Task {
            output,
            worker: 2,
            deadline: 100,
            reads: 0,
            stale: false,
            sources: vec![SourceStamp {
                actor: source,
                digest: [1; 32],
            }],
            published: Some(([2; 32], 200)),
        };
        let engine = Engine::new(
            1,
            crate::Limits::default(),
            crate::MemoryJournal::new(1024),
            1,
            std::sync::Arc::new(ciagomem_core::MonotonicClock),
        )
        .unwrap();
        let mut broker = MemoryBroker {
            engine,
            tenant: 11,
            controller: 1,
            audience: 7,
            id: 1,
            next: 3,
            limits: MemoryLimits::default(),
            tasks: [
                (1, task(actor(10, 100), actor(1, 101))),
                (2, task(actor(11, 100), actor(10, 100))),
            ]
            .into(),
        };
        // Simulate a future trusted replacement event. Today's public ingest rejects
        // changed incarnations; this tests the invalidation boundary independently.
        broker.invalidate(actor(1, 999));
        assert!(broker.tasks[&1].stale);
        assert!(broker.tasks[&2].stale);
    }
}
