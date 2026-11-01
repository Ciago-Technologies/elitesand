# MemoryBroker audit input — readable UTF-8 source

This is a plain text review artifact, not a ZIP, binary, Git dump or executable. Start with the complete implementation below, followed by exact specification Section18. Dependency excerpts, complete tests/example and research are supplied separately in MEMORY_BROKER_AUDIT_CONTEXT.md. This delivery contains the corrected implementation/specification described in docs/MEMORY_STALENESS_REMEDIATION.md.

## Facts the audit must not assume

- The original ZIP is valid and contains readable UTF-8 Rust source. Its memory.rs matches the current workspace; there are no .git directories or Git .pack/.idx files in it. Tools that show the compressed archive itself need extraction or these text files.
- IdentityFile, the persisted 72-byte network result ledger, document_version CAS and restart-safe task restoration are target contracts. The current broker has live-task digest idempotency, opaque local leases, private-engine revision invalidation and volatile task metadata. Do not assume it integrates the future client ledger.
- Source digests are captured once at delegation. Controlled source writes/forget mark dependent tasks stale transitively. Source freshness does not map to a persisted document_version in this implementation; external file changes must be reported by a trusted adapter.
- The 22037-to-210 byte example is deterministic error-line filtering. It is not evidence of LLM token savings, summary accuracy, hallucination resistance, model-worker economics or production latency.
- External taint and missing frame/action authority are enforced by the actual broker/frame paths when used correctly. They do not guarantee every model inference is correct or every host integration is secure. Native plugins are not isolated by Rust API privacy; trusted issuer verification and sandboxed model tools are required.
- The correction run passed186 default /214 all-feature tests, including19 broker integration cases and the defensive replacement-event unit test. Three real io_uring bodies skipped. Source blocks below are exact copies of the corrected files.

Review scope: authorization/issuer assumptions; controller and worker roles; source/output reservation; transitive revision and TTL invalidation; parent-summary expiry; journal failure and live-task retry behavior; byte/read/task bounds; retained snapshots; provenance downgrade/admission; and future-service boundaries. Return concrete findings with source location, reproduction, smallest correction and acceptance test. Treat specification promises separately from implemented code.

## ciagomem-engine/src/memory.rs

SHA256: `888f586861f32e6760234c00818a3f721c4b8d3df4267d0f12aaa60926bceb6a`. Complete file, unchanged.

````rust
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
````

## Exact Section18 from MASTER_SPECIFICATION.md

SHA256 of the full specification: `22c18c50cc07bb53c86f0d252f2c1e49ace08af5551816474f6c24f865ee2c32`. The following section is copied verbatim.

## 18. Sub-agent memory orchestration and hybrid context assembly

### 18.1 Purpose and economics

The "Zero-Token Waste Pattern" means avoiding unnecessary repetition in the expensive main agent's context. It does **not** mean zero inference tokens or zero cost. The worker reads allowed evidence, applies deterministic preprocessing or a suitable model, publishes a compact summary, and hands the orchestrator a reference. The orchestrator reads only an authorized context slice unless an explicit evidence-verification escalation is needed. Model names, prices and guaranteed percentage savings are not part of this contract.

```text
Main agent -- scoped delegation/budgets --> Worker
     ^                                      |
     |                                      | approved files/logs/history
     |                                      | parse, extract, summarize
     |                                      v
     +-- bounded cited context <-- Ciagomem memory broker/store
                   ^              | sources, digests, labels, TTL
                   +-- prior notes/frames + exact/text/vector candidates
```

Evidence-to-summary compression is useful only if it preserves task quality and reduces total measured cost. Define baseline_cost as actual main input/output, tools and storage cost for the same workload; hybrid_cost includes main input/output, worker input/output, embedding/indexing, retrieval, provider caching, retries, verification and orchestration/storage cost. Savings=(baseline_cost-hybrid_cost)/baseline_cost, only when baseline_cost>0. Never equate a reduction in main input tokens with total token or monetary savings. Worker parallelism may increase total usage and queue latency. Reuse source-versioned summaries across turns only while fresh and authorized; cap worker fanout, calls, retries and output before invoking a model. Prefer deterministic filtering/deduplication and cached summaries when sufficient.

Research and threat decisions are recorded in [MEMORY_ORCHESTRATION_RESEARCH.md](docs/MEMORY_ORCHESTRATION_RESEARCH.md). Primary evidence: [Anthropic multi-agent engineering](https://www.anthropic.com/research/multi-agent-research-system), [context engineering](https://www.anthropic.com/research/effective-context-engineering-for-ai-agents), [CaMeL](https://arxiv.org/abs/2503.18813) and [source/sink prompt-injection defenses](https://openai.com/index/designing-agents-to-resist-prompt-injection/). These sources motivate the design; they do not certify Ciagomem or supply its performance measurements.

### 18.2 Implemented Rust component and authority boundary

`ciagomem-engine::memory::MemoryBroker<J>` is a bounded **in-process reference implementation**, owning an existing Engine and using its actor FlatBuffer images, journal, authorization and labels. It is not a model runtime, network server, scheduler, sandbox or file reader. No new dependency, persistent header field, unsafe code or CiaQL grammar change is required for this component. Sections14–17 remain the target host/collection/SDK contract, not completed features.

The broker binds one tenant incarnation, controller session, audience and random broker ID. VerifiedMacaroon inputs must have been validated by the trusted host against its configured issuer and authorization context; arbitrary co-resident Rust code can issue/verify values with its own keys, so the type alone is not an issuer boundary. A model receives only scoped host tool wrappers, never native constructors or signing access. A distinct verified worker session receives an opaque local WorkerLease for exact source ActorRefs and a fresh output ActorRef. The broker privately owns the Engine: worker callers cannot use general Engine put/query/journal access through it. The trusted host must not retain another unrestricted authority path or give the worker an issuer key. Workers in an untrusted process must receive only broker-mediated transport APIs; Rust type privacy is not an OS sandbox or an authenticated remote capability codec.

API sequence:

1. `new(engine, controller, limits)` checks current token/boot/expiry, controller read+write rights and limits.
2. `ingest(record, controller)` admits trusted-adapter-supplied source records. It performs no path/URL access. External files/logs receive External labels. Reserved memory-output IDs cannot be overwritten through ingestion.
3. `delegate(output, sources, worker, controller)` validates both session scopes, source readers/full incarnations, unique source/output IDs, quotas and deadline. It captures each verified source image's SHA256 digest once. The output must not already exist or belong to another live task.
4. `read_sources(lease, worker)` returns only those authorized current snapshots under the configured source-read/image-byte bounds. These inputs go to the worker, never automatically to the main model.
5. `publish(lease, summary, worker)` constructs a summary Record with structured source refs/digests and a derived label. The summary is data, never evaluated CiaQL. Journal commit precedes task publication. Identical live-task retries return the same MemoryRef without another write only while the task is non-stale and its publication TTL is live. After authorization, an invalidated task or expired publication returns StaleReference before any cached-success return; a changed summary for an otherwise valid publication returns Duplicate.
6. `result(lease, controller)` learns a publication reference after a lost completion message. It is reconciliation metadata, not proof the referenced content is still fresh.
7. `read_context(refs, byte_budget, controller)` checks published state, scopes, source validity and TTL, deduplicates repeated refs, and packs whole summaries in the supplied priority order. The returned ContextSlice contains untrusted text, propagated Label, structured source citations and an omitted count. Framing/citation bytes count against the byte limit. A budget too small for every item returns Budget; no partial UTF-8/string truncation occurs.
8. `forget(lease, controller)` journal-deletes an existing output before releasing task metadata/reservation. Pending tasks can be cancelled without publishing data. Failed journal cleanup retains the task so it can be retried.

| Bound | Default | Constructor ceiling |
| --- | ---: | ---: |
| Live pending+published tasks | 64 | 1024 |
| Sources per task | 16 | 32 |
| Source image bytes per fetch/task | 1048576 | 16777216 |
| Source reads per task | 1 | 16 |
| Summary UTF-8 bytes | 4096 | 16384 |
| Serialized context bytes | 8192 | 65536 |
| Context references per call | 8 | 64 |
| Worker lease duration, ns | 300000000000 | 300000000000 |
| Summary TTL, ns | 900000000000 | 3600000000000 |

All bounds must be nonzero. Engine row/field/image/index/journal limits apply additionally and can reject earlier. Task limits are per broker; the host must bound broker instances, model work and outstanding response/snapshot ownership per tenant. Returned Arc snapshots can retain memory in caller code; this library cannot enforce process RSS against a hostile caller holding arbitrary Rust values. Output bytes are not provider tokens. A trusted provider tokenizer/billing adapter must enforce actual token/cost budgets including system prompts, tools and output reservations; never trust a worker-reported usage count alone.

### 18.3 Freshness, concurrency and failure behavior

Broker mutation is owner-serialized. Only trusted ingestion or publication/forget can change its privately held Engine. A successful source ingest conservatively marks every dependent task stale by actor ID, including transitive summary descendants and dependencies on any earlier incarnation of that ID, even if a rewrite is byte-identical. Matching by ID applies only to invalidation/revocation: source resolution and authority still require the complete ActorRef. A failed journal operation does not invalidate the old visible state. Delegation rejects stale/expired parent summaries; derived summary TTL is capped by the earliest source-summary TTL. Forget invalidates descendants before removing the parent reservation. Thus context reads validate revision fences without rehashing raw multi-page input on each main turn; initial source digests remain immutable citation evidence.

External file changes are unknown until the trusted ingestion adapter reports a new source revision. A filesystem path is not a version proof. Target adapters must bind immutable object/version ID, content hash, allowed root, byte/line range, parser version and extraction configuration; verify them before ingestion. Do not claim freshness for unreported external changes. Source deletion/incarnation changes must use the same invalidation path before visibility/reclamation; the current broker exposes controlled ingestion/forget, not arbitrary deletion/query mutation.

Current Engine::put rejects an attempt to replace an existing actor ID with a different incarnation before committing or installing it. Therefore the reported public ingest-based incarnation-replacement exploit is blocked today. ID-based invalidation is additional defensive protection for a future trusted replacement event, tested independently; do not weaken Engine incarnation checks to simulate that exploit.

Pending publication is visible only after Engine journal success; a matching retry checks scope/lease expiry, task staleness and publication TTL before returning its prior reference. result() remains a historical completion-metadata lookup: it can return a prior reference after invalidation, but read_context must reject consuming it as fresh evidence. Broker state transitions are Pending->Published or removal by Forget, never two different published summaries for one task. Worker leases use the Engine's trusted nondecreasing clock and an absolute deadline bounded by current worker/controller token expiry and any source-summary expiry; token renewal cannot extend that deadline. Expired tasks remain charged until controller cleanup. Summary expiration is separate from task deadline. A late journal completion may be committed even if the client deadline elapsed: reconcile, never automatically issue a different semantic job as if the first had aborted.

The current broker/task table, cache and live-task retry state are volatile. MemoryJournal is non-durable. A supplied Journal is responsible for its actual boundary, and this change does not provide the durable node replay/dedup driver. After restart, the new random broker ID invalidates old leases/references; there is no automatic restoration of task metadata from summary images. Target production recovery must atomically journal task identity/state, source manifest, publication version, result/idempotency record and cancellation progress before acknowledging durable outcomes. Persist/refuse replay according to14.6/17.6; never turn current in-process idempotency into an exactly-once-across-restart claim.

### 18.4 Provenance, scratch isolation and prompt-injection containment

Worker origin and integrity are different properties. Logs, arbitrary files, scraped pages and Q-LLM text are External. Derived output inherits the minimum source integrity, source-category union and reader-set intersection using existing Label::derived. A model designated "worker" cannot select its own Label or promote External evidence to AgentDerived. All-trusted parents may yield derived AgentDerived metadata, but derivation clears issuer authority: that is not a signed Fact/Policy admission. Existing content-bound attestation/endorsement and single-use frame/tool permit requirements still apply. No wrapper, Markdown delimiter or summary format proves an instruction harmless.

ContextSlice.evidence_label() explicitly downgrades unsigned derived metadata to External while preserving source categories and reader intersection, allowing the existing Var/data admission path to verify the exact assembled text. It never creates a Fact or action permit. Read summaries as data in a permitted low-trust memory-frame slot; never concatenate them into system policy, ActiveIntent or trusted playbooks. ContextSlice.label plus its exact content must pass the existing appropriate admission path; Fact/Policy/sensitive tool actions require their stronger evidence and permits. Trusted fixed policy/user instructions remain separate from retrieved evidence. A malicious summary can still affect model reasoning; source/sink enforcement restricts effects, not every erroneous inference. Grounding/completeness needs evaluation and explicit source inspection for high-impact decisions.

Target deployments isolate raw evidence, worker scratchpads, published agent memories and user application data into separately authorized namespace/family scopes. Share memory by explicit broker-mediated publication/authority, not a tenant-wide scan token. The current broker enforces tenant/session/exact-source/output isolation; it does not implement the future namespace/family Macaroon caveats or SchemaCatalog. Cross-namespace copying requires an explicit authenticated export/endorsement/declassification design, not simply storing a common actor ID.

Workers receive only the allowed evidence reader and scoped publisher. Filesystem/network/tool adapters require allowlisted immutable resources, no symlink/traversal escape, bounded decompression/tokenization, redacted secrets and no arbitrary code/command execution. Source citations are references, not an instruction to auto-fetch arbitrary URLs. A Markdown summary can be stored as a bounded scalar string. File materialization is an optional hardened trusted export adapter, not arbitrary worker writes into project/policy directories.

### 18.5 Hybrid retrieval, delta arrays and prior memory frames

Combine three layers: trusted fixed constraints/intent; compact validated prior notes or task summaries; just-in-time evidence retrieval when new information or uncertainty warrants it. Existing seqlock snapshots, Label joins and frame/action admission primitives remain the lower-level enforcement. File parsing, decompression, model/API calls and large-source hashing must be scheduled outside latency-critical reactor work. The current reference broker hashes bounded source images synchronously at delegation and is not a qualified hot-reactor service; a production adapter should capture trusted ingestion fingerprints under bounded staging/tick work. The implemented broker takes explicit MemoryRefs in host-chosen rank order and returns a bounded slice; it does not run an automatic named-family hybrid search planner.

Target `agent_scratch`, `agent_memory` and task/history families use14's BFBS catalog and typed CRUD. A summary is a scalar searchable field, one compact document per episode/task; do not pass a `[string]` history vector as a scalar SEARCH field. Exact key/ref filters precede bounded BM25 and optional SQ8/exact vector reranking. Apply namespace/family/session scope and source revision/TTL checks before ranking, not after exposing candidates. Search results remain candidates until the broker validates current authority/freshness and assembles the context budget. Match embedding dimension/model version and tokenizer/extraction version in cache identity.

A cache key binds tenant/namespace, task objective digest, full source manifest/revisions, extraction prompt/parser/model version, embedding version, requested output shape and policy epoch. Invalidate on source/permission/policy changes; never reuse a better-privileged user's summary for another principal. The current broker's explicit job identity does not implement this semantic cache directory or model-call coalescing.

The target worker writes scalar updates and bounded history refs with compact delta operations such as `push_bounded`; it cannot mutate a generated FlatBuffers vector in place. Overlay records, scratch compaction, class promotion, CAS version checks and WAL/result ledger follow14–17. The current reference broker publishes complete fixed-format Record images and does not implement delta tails or general arrays. It reduces main-context repetition; it does not make model inference/local IPC/commits sub-microsecond. Markdown text, scalar summaries and optional vector embeddings still obey extent and retention limits; raw archives belong in a bounded external evidence store.

### 18.6 Acceptance, operations and cost qualification

`ciagomem-engine/tests/memory.rs` covers scoped sessions, forged/cross-broker leases/refs, publication conflicts, journal rejection, byte/source/read/task budgets, Unicode packing, source revision and transitive invalidation, parent TTL, forget and taint propagation. The runnable `memory_orchestration` example uses deterministic error-line extraction as the cheapest worker baseline; it performs no external model/API call. Its byte counts are not token/cost/latency measurements.

Before a production service claim, add persisted task-state/restart failure injection, multiple worker lease fencing, real capability/namespace transport integration, source-adapter path/decompression attacks, secret/PII leakage tests, stale-cache/permission revocation, malicious-summary tool-exfiltration attempts, grounding/factual recall evaluations, provider usage verification, cost/fanout/deadline admission and memory retention pressure. Include adversarial summaries that forge delimiters, citations or policy instructions; reject authority escalation even when text is plausible.

Collect actual worker/main input and output tokens (including cached rates and retries), model/tool calls, extraction/citation quality, stale/omitted contexts, context bytes, lease expiry/retries, journal failures, provider queue/runtime and total p50/p99 end-to-end latency. Test cold and warm caches, changed-source invalidation, repeated turns and tasks with little compressible context. Compare direct main access, deterministic preprocessing, single-worker and hybrid retrieval at equivalent answer quality. Neither "90%+" nor a universal70–90% reduction is promised. If delegation costs more or loses required evidence, choose a cheaper direct/preprocessing path or escalate within an explicit budget.
