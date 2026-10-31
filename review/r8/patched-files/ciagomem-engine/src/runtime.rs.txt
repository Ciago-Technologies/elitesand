use crate::{
    index::{Index, Sq8, literal},
    links::{Edge, Links, OnDelete},
    schema::Schemas,
    *,
};
use ciagomem_core::{AuthContext, Label, OP_READ, OP_WRITE, VerifiedMacaroon, validate_query};
use ciagomem_ql::{
    AggFunc, AggItem, AssignOp, BoolExpr, CompOp, FieldPath, Metric, Mutation, Query, Root, Source,
    Stage, Value,
};
use roaring::RoaringBitmap;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[derive(Debug, Clone)]
pub enum Change {
    Put(Snapshot),
    Delete(ActorRef),
    Link(Edge),
    BeginDelete { job: u64, plan: Vec<ActorRef> },
    DeleteProgress { job: u64, cursor: u64 },
}
#[derive(Debug, Clone)]
pub struct Batch {
    pub sequence: u64,
    pub changes: Vec<Change>,
}
/// A production adapter must append a complete batch and establish its durability boundary
/// before returning success. The built-in MemoryJournal deliberately makes no durability claim.
pub trait Journal {
    fn commit(&mut self, batch: &Batch) -> Result<()>;
}
pub struct MemoryJournal {
    pub batches: Vec<Batch>,
    limit: usize,
    used: usize,
}
impl MemoryJournal {
    pub fn new(limit: usize) -> Self {
        Self {
            batches: Vec::new(),
            limit,
            used: 0,
        }
    }
}
impl Journal for MemoryJournal {
    fn commit(&mut self, b: &Batch) -> Result<()> {
        let bytes = b.changes.iter().try_fold(64usize, |n, c| {
            n.checked_add(match c {
                Change::Put(s) => {
                    s.image.len()
                        + 512
                        + s.record
                            .fields
                            .iter()
                            .map(|(k, v)| k.len() + v.bytes() + 64)
                            .sum::<usize>()
                        + s.record.embedding.len() * 4
                }
                Change::BeginDelete { plan, .. } => 128 + plan.len() * 32,
                _ => 128,
            })
            .ok_or(Error::Budget)
        })?;
        if bytes > self.limit.saturating_sub(self.used) {
            return Err(Error::Journal);
        }
        self.batches.push(b.clone());
        self.used += bytes;
        Ok(())
    }
}
struct Stored {
    slot: u32,
    snapshot: Snapshot,
    charge: usize,
}
#[derive(Default)]
struct Shard {
    rows: BTreeMap<u64, Stored>,
    index: BTreeMap<u128, Index>,
    by_slot: BTreeMap<u32, u64>,
    free: Vec<u32>,
    next: u32,
    bytes: usize,
}
pub struct Engine<J: Journal> {
    boot_epoch: u128,
    clock: Arc<dyn ciagomem_core::Clock + Send + Sync>,
    last_now: std::sync::atomic::AtomicU64,
    shards: Vec<Shard>,
    limits: Limits,
    journal: J,
    sequence: u64,
    links: Links,
    deleting: BTreeSet<ActorRef>,
    jobs: BTreeMap<u64, DeleteJob>,
    pub schemas: Schemas,
}
#[derive(Debug, Clone)]
pub struct ResultRow {
    pub actor: Option<ActorRef>,
    pub values: Fields,
    pub joins: BTreeMap<String, Fields>,
    pub label: Label,
    pub score: Option<f32>,
}
#[derive(Debug, Default, Clone)]
pub struct QueryStats {
    pub work: usize,
    pub bitmap_candidates: u64,
    pub joined: usize,
    pub returned_bytes: usize,
    pub approximate: bool,
}
#[derive(Debug, Clone)]
pub struct QueryResult {
    pub rows: Vec<ResultRow>,
    pub stats: QueryStats,
}
#[derive(Debug, Default, Clone, Copy)]
pub struct Options {
    pub approximate_candidates: Option<usize>,
    pub disable_indexes: bool,
}
#[derive(Clone)]
struct Work {
    record: Option<Arc<Record>>,
    values: Option<Fields>,
    joins: BTreeMap<String, Arc<Record>>,
    label: Label,
    score: Option<f32>,
}
impl Work {
    fn new(record: Arc<Record>) -> Self {
        Self {
            values: None,
            label: record.label.clone(),
            record: Some(record),
            joins: BTreeMap::new(),
            score: None,
        }
    }
    fn actor(&self) -> u64 {
        self.record.as_ref().map_or(0, |r| r.actor.id)
    }
    fn field(&self, p: &FieldPath<'_>) -> Result<Scalar> {
        if self.record.is_none() && p.alias.is_none() && p.actor.is_none() && p.root == Root::State
        {
            return Ok(self
                .values
                .as_ref()
                .and_then(|v| v.get(&p.path.join(".")))
                .cloned()
                .unwrap_or(Scalar::Null));
        }
        let record = if let Some(alias) = p.alias {
            self.joins.get(alias).ok_or(Error::InvalidQuery)?
        } else {
            self.record.as_ref().ok_or(Error::Unsupported)?
        };
        if p.actor.is_some_and(|id| id != record.actor.id) {
            return Err(Error::Unsupported);
        }
        match p.root {
            Root::State => Ok(record
                .fields
                .get(&p.path.join("."))
                .cloned()
                .unwrap_or(Scalar::Null)),
            Root::Meta if p.path == ["schema_version"] => {
                Ok(Scalar::Int(record.schema_version as i64))
            }
            _ => Err(Error::Unsupported),
        }
    }
}
fn charge(stats: &mut QueryStats, n: usize, limit: usize) -> Result<()> {
    stats.work = stats.work.checked_add(n).ok_or(Error::Budget)?;
    if stats.work > limit {
        Err(Error::Budget)
    } else {
        Ok(())
    }
}
fn allowed(token: &VerifiedMacaroon, r: &Record, write: bool) -> Result<()> {
    if r.tenant != token.tenant_incarnation() {
        return Err(Error::ForeignActor);
    }
    if !token.allows(if write { OP_WRITE } else { OP_READ })
        || !r.label.readers().contains(&token.tenant_incarnation())
    {
        return Err(Error::Permission);
    }
    Ok(())
}
impl<J: Journal> Engine<J> {
    pub fn new(
        cores: usize,
        limits: Limits,
        journal: J,
        boot_epoch: u128,
        clock: Arc<dyn ciagomem_core::Clock + Send + Sync>,
    ) -> Result<Self> {
        if boot_epoch == 0
            || cores == 0
            || cores > 64
            || limits.rows_per_core == 0
            || limits.rows_per_core > u32::MAX as usize
            || limits.vector_dims > 4096
            || limits.fields == 0
        {
            return Err(Error::InvalidValue);
        }
        Ok(Self {
            boot_epoch,
            clock,
            last_now: std::sync::atomic::AtomicU64::new(0),
            shards: (0..cores).map(|_| Shard::default()).collect(),
            limits,
            journal,
            sequence: 0,
            links: Links::default(),
            deleting: BTreeSet::new(),
            jobs: BTreeMap::new(),
            schemas: Schemas::new(),
        })
    }
    fn authorize(&self, token: &VerifiedMacaroon) -> Result<()> {
        // R8-3: Send + Sync high-water mark (Engine was Send + Sync before R7-5).
        let seen = self.clock.now_ns();
        let now = seen.max(
            self.last_now
                .fetch_max(seen, std::sync::atomic::Ordering::AcqRel),
        );
        if token.boot_epoch() != self.boot_epoch || now >= token.expires_at() {
            Err(Error::Permission)
        } else {
            Ok(())
        }
    }
    pub fn owner_core(&self, id: u64) -> usize {
        (id % self.shards.len() as u64) as usize
    }
    pub fn journal(&self) -> &J {
        &self.journal
    }
    pub fn accounted_bytes(&self) -> usize {
        self.shards.iter().map(|s| s.bytes).sum()
    }
    fn stored(&self, id: u64) -> Result<&Stored> {
        self.shards[self.owner_core(id)]
            .rows
            .get(&id)
            .ok_or(Error::MissingActor)
    }
    pub fn snapshot(&self, actor: ActorRef, token: &VerifiedMacaroon) -> Result<Snapshot> {
        self.authorize(token)?;
        let s = self.stored(actor.id)?;
        allowed(token, &s.snapshot.record, false)?;
        if self.deleting.contains(&actor) {
            return Err(Error::Restricted);
        }
        if s.snapshot.record.actor != actor {
            return Err(Error::StaleReference);
        }
        Ok(s.snapshot.clone())
    }
    pub fn handle_join(
        &self,
        r: &join::Request,
        token: &VerifiedMacaroon,
        now: u64,
    ) -> join::Reply {
        let snapshot = if self.authorize(token).is_err() {
            Err(Error::Permission)
        } else if r.tenant != token.tenant_incarnation() {
            Err(Error::ForeignActor)
        } else if now >= r.deadline_ns {
            Err(Error::Budget)
        } else {
            self.snapshot(r.target, token)
        };
        join::Reply {
            ticket: r.ticket,
            target: r.target,
            snapshot,
        }
    }
    fn commit(&mut self, changes: Vec<Change>) -> Result<()> {
        let seq = self.sequence.checked_add(1).ok_or(Error::Overflow)?;
        self.journal.commit(&Batch {
            sequence: seq,
            changes,
        })?;
        self.sequence = seq;
        Ok(())
    }
    fn prepare(&self, record: Record) -> Result<(Snapshot, usize)> {
        if record.schema_version > self.schemas.latest() {
            return Err(Error::MissingSchema);
        }
        let cost = self.limits.validate(&record)?;
        let bytes = image::encode(&record);
        image::view(&bytes)?;
        let cost = cost.checked_add(bytes.len()).ok_or(Error::Budget)?;
        let shard = &self.shards[self.owner_core(record.actor.id)];
        let old = shard.rows.get(&record.actor.id);
        if old.is_some_and(|s| s.snapshot.record.actor != record.actor) {
            return Err(Error::StaleReference);
        }
        if old.is_some_and(|s| s.snapshot.record.tenant != record.tenant) {
            return Err(Error::ForeignActor);
        }
        if old.is_none() && shard.rows.len() >= self.limits.rows_per_core {
            return Err(Error::Budget);
        }
        if cost
            > self
                .limits
                .bytes_per_core
                .saturating_sub(shard.bytes - old.map_or(0, |s| s.charge))
        {
            return Err(Error::Budget);
        }
        Ok((
            Snapshot {
                record: Arc::new(record),
                image: bytes.into(),
            },
            cost,
        ))
    }
    fn install(&mut self, snapshot: Snapshot, cost: usize) {
        let shard_id = self.owner_core(snapshot.record.actor.id);
        let shard = &mut self.shards[shard_id];
        let old = shard.rows.remove(&snapshot.record.actor.id);
        let slot = if let Some(old) = old {
            shard
                .index
                .get_mut(&old.snapshot.record.tenant)
                .unwrap()
                .remove(old.slot, &old.snapshot.record);
            shard.bytes -= old.charge;
            old.slot
        } else {
            shard.free.pop().unwrap_or_else(|| {
                let slot = shard.next;
                shard.next += 1;
                slot
            })
        };
        shard
            .index
            .entry(snapshot.record.tenant)
            .or_default()
            .insert(slot, &snapshot.record);
        shard.by_slot.insert(slot, snapshot.record.actor.id);
        shard.bytes += cost;
        shard.rows.insert(
            snapshot.record.actor.id,
            Stored {
                slot,
                snapshot,
                charge: cost,
            },
        );
    }
    pub fn put(&mut self, record: Record, token: &VerifiedMacaroon) -> Result<()> {
        self.authorize(token)?;
        allowed(token, &record, true)?;
        if self.deleting.contains(&record.actor) {
            return Err(Error::Restricted);
        }
        let (s, c) = self.prepare(record)?;
        self.commit(vec![Change::Put(s.clone())])?;
        self.install(s, c);
        Ok(())
    }
    pub fn add_link(
        &mut self,
        dependent: ActorRef,
        target: ActorRef,
        mode: OnDelete,
        token: &VerifiedMacaroon,
    ) -> Result<()> {
        self.authorize(token)?;
        if !token.allows(OP_WRITE) {
            return Err(Error::Permission);
        }
        self.snapshot(dependent, token)?;
        self.snapshot(target, token)?;
        if !self.links.contains(dependent, target)
            && self.links.count() >= self.limits.rows_per_core * self.shards.len() * 4
        {
            return Err(Error::Budget);
        }
        let e = Edge {
            dependent,
            target,
            on_delete: mode,
        };
        self.commit(vec![Change::Link(e)])?;
        self.links.add(e);
        Ok(())
    }
    pub fn delete(&mut self, actor: ActorRef, token: &VerifiedMacaroon) -> Result<usize> {
        self.authorize(token)?;
        if !token.allows(OP_WRITE) {
            return Err(Error::Permission);
        }
        self.snapshot(actor, token)?;
        let plan = self.links.plan_delete(actor, self.limits.cascade_rows)?;
        for a in &plan {
            self.snapshot(*a, token)?;
        }
        self.commit(plan.iter().copied().map(Change::Delete).collect())?;
        for actor in &plan {
            let core = self.owner_core(actor.id);
            let shard = &mut self.shards[core];
            let old = shard.rows.remove(&actor.id).expect("validated deletion");
            shard
                .index
                .get_mut(&old.snapshot.record.tenant)
                .unwrap()
                .remove(old.slot, &old.snapshot.record);
            shard.bytes -= old.charge;
            if shard
                .index
                .get(&old.snapshot.record.tenant)
                .is_some_and(|i| i.all.is_empty())
            {
                shard.index.remove(&old.snapshot.record.tenant);
            }
            shard.by_slot.remove(&old.slot);
            shard.free.push(old.slot);
            self.links.remove_actor(*actor);
        }
        Ok(plan.len())
    }
    pub fn projected(&self, actor: ActorRef, token: &VerifiedMacaroon) -> Result<Record> {
        self.authorize(token)?;
        self.schemas.project(&self.snapshot(actor, token)?.record)
    }
    pub fn upgrade(&mut self, actor: ActorRef, token: &VerifiedMacaroon) -> Result<bool> {
        self.authorize(token)?;
        let s = self.snapshot(actor, token)?;
        if s.record.schema_version == self.schemas.latest() {
            return Ok(false);
        }
        let r = self.schemas.project(&s.record)?;
        self.put(r, token)?;
        Ok(true)
    }
    pub fn execute(&mut self, q: &Query<'_>, token: &VerifiedMacaroon) -> Result<QueryResult> {
        self.authorize(token)?;
        self.execute_with(q, token, Options::default())
    }
    pub fn execute_with(
        &mut self,
        q: &Query<'_>,
        token: &VerifiedMacaroon,
        options: Options,
    ) -> Result<QueryResult> {
        self.authorize(token)?;
        let owner = |id| self.stored(id).ok().map(|s| s.snapshot.record.tenant);
        let validated = validate_query(
            q,
            &AuthContext {
                session: token,
                owner_of: &owner,
                allowed_destinations: &[],
                owner_version: self.sequence,
            },
        )
        .map_err(|_| Error::Permission)?;
        self.execute_validated_with(validated, token, options)
    }
    pub fn execute_validated_with(
        &mut self,
        validated: ciagomem_core::ValidatedQuery<'_, '_>,
        token: &VerifiedMacaroon,
        options: Options,
    ) -> Result<QueryResult> {
        self.authorize(token)?;
        if validated.scope() != ciagomem_core::QueryScope::Tenant(token.tenant_incarnation())
            || validated.session_id() != token.session_id()
            || validated.owner_version() != self.sequence
        {
            return Err(Error::StaleReference);
        }
        let q = validated.query();
        if options.approximate_candidates == Some(0) {
            return Err(Error::InvalidValue);
        }
        // Compile supported combinations before any side effect or partial journal entry.
        let writes = q
            .stages
            .iter()
            .filter(|s| matches!(s, Stage::Mutate(_) | Stage::LinkAdd { .. }))
            .count();
        if writes > 1
            || writes > 0
                && (!matches!(q.source, Source::Actor(_))
                    || q.stages.iter().any(|s| {
                        matches!(
                            s,
                            Stage::JoinActor { .. }
                                | Stage::GroupBy(_)
                                | Stage::Reduce(_)
                                | Stage::SearchBm25 { .. }
                                | Stage::Similarity { .. }
                        )
                    }))
        {
            return Err(Error::Unsupported);
        }
        if q.stages.iter().any(|s| {
            matches!(
                s,
                Stage::CheckpointFrame { .. } | Stage::HydrateFrame { .. } | Stage::ExportTo { .. }
            )
        }) {
            return Err(Error::Unsupported);
        }
        let mut stats = QueryStats::default();
        let mut rows = Vec::new();
        let mut index_work = 0usize;
        let mut scratch = 0usize;
        let mut push = |s: &Stored| -> Result<()> {
            charge(&mut stats, 1, self.limits.query_work)?;
            if s.snapshot.record.tenant != token.tenant_incarnation() {
                return Ok(());
            }
            allowed(token, &s.snapshot.record, false)?;
            if self.deleting.contains(&s.snapshot.record.actor) {
                return Ok(());
            }
            scratch = scratch.saturating_add(256);
            if s.snapshot.record.schema_version != self.schemas.latest() {
                scratch = scratch.saturating_add(s.charge);
            }
            if scratch > self.limits.result_bytes.saturating_mul(4) {
                return Err(Error::Budget);
            }
            let record = if s.snapshot.record.schema_version == self.schemas.latest() {
                s.snapshot.record.clone()
            } else {
                let projected = self.schemas.project(&s.snapshot.record)?;
                self.limits.validate(&projected)?;
                if self.limits.validate(&projected)? > self.limits.result_bytes.saturating_mul(4) {
                    return Err(Error::Budget);
                }
                Arc::new(projected)
            };
            rows.push(Work::new(record));
            Ok(())
        };
        match &q.source {
            Source::Actor(id) => push(self.stored(*id)?)?,
            Source::TxActors(ids) => {
                for id in ids {
                    push(self.stored(*id)?)?
                }
            }
            Source::ScanAll | Source::ScanRange { .. } => {
                for shard in &self.shards {
                    let Some(index) = shard.index.get(&token.tenant_incarnation()) else {
                        continue;
                    };
                    let candidates = if !options.disable_indexes && self.schemas.latest() == 1 {
                        if let Some(Stage::Filter(e)) = q.stages.first() {
                            index_work = index_work.saturating_add(index.candidate_work(e));
                            if index_work > self.limits.query_work {
                                return Err(Error::Budget);
                            }
                            index.candidates(e)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let bits = candidates.as_ref().unwrap_or(&index.all);
                    if bits.len() as usize > self.limits.query_work {
                        return Err(Error::Budget);
                    }
                    for slot in bits.iter() {
                        let id = shard.by_slot[&slot];
                        if !matches!(q.source,Source::ScanRange {start,end} if id<start||id>end) {
                            push(&shard.rows[&id])?;
                        }
                    }
                }
            }
            Source::BackupStore { .. } => return Err(Error::Unsupported),
        }
        charge(&mut stats, index_work, self.limits.query_work)?;
        stats.bitmap_candidates = rows.len() as u64;
        rows.sort_by_key(Work::actor);
        let mut group_paths = None;
        let mut pending_put = None;
        let mut pending_link = None;
        for stage in &q.stages {
            match stage {
                Stage::Filter(e) => {
                    let indexed: Option<Vec<Option<RoaringBitmap>>> = None;
                    let mut kept = Vec::new();
                    for w in rows {
                        if let (Some(maps), Some(record)) = (&indexed, &w.record) {
                            let core = self.owner_core(record.actor.id);
                            if let Some(bits) = &maps[core] {
                                let slot = self.stored(record.actor.id)?.slot;
                                if !bits.contains(slot) {
                                    continue;
                                }
                                stats.bitmap_candidates += 1;
                            }
                        }
                        charge(&mut stats, 1, self.limits.query_work)?;
                        if evaluate(e, &w)? {
                            kept.push(w)
                        }
                    }
                    rows = kept;
                }
                Stage::JoinActor { target, alias } => {
                    for w in &mut rows {
                        charge(&mut stats, 1, self.limits.query_work)?;
                        stats.joined += 1;
                        if stats.joined > self.limits.join_rows {
                            return Err(Error::Budget);
                        }
                        let Scalar::Ref(target) = w.field(target)? else {
                            return Err(Error::Type);
                        };
                        let snapshot = self.snapshot(target, token)?;
                        let record = if snapshot.record.schema_version == self.schemas.latest() {
                            snapshot.record.clone()
                        } else {
                            Arc::new(self.schemas.project(&snapshot.record)?)
                        };
                        w.label = w.label.join(&record.label);
                        w.joins.insert((*alias).to_owned(), record);
                    }
                }
                Stage::Mutate(mutations) => {
                    if rows.is_empty() {
                        continue;
                    }
                    if rows.len() != 1 {
                        return Err(Error::Unsupported);
                    }
                    let mut record = (*rows[0].record.as_ref().unwrap().as_ref()).clone();
                    for m in mutations {
                        let Mutation::Assign { target, op, value } = m else {
                            return Err(Error::Unsupported);
                        };
                        let Value::Lit(v) = value else {
                            return Err(Error::Unsupported);
                        };
                        let v = literal(*v)?;
                        let name = target.path.join(".");
                        let new = match op {
                            AssignOp::Set => v,
                            AssignOp::Add | AssignOp::Sub => {
                                let old = record.fields.get(&name).ok_or(Error::Type)?;
                                match (old, v) {
                                    (Scalar::Int(a), Scalar::Int(b)) => Scalar::Int(
                                        if *op == AssignOp::Add {
                                            a.checked_add(b)
                                        } else {
                                            a.checked_sub(b)
                                        }
                                        .ok_or(Error::Overflow)?,
                                    ),
                                    (a, b) => {
                                        let value = if *op == AssignOp::Add {
                                            a.number()? + b.number()?
                                        } else {
                                            a.number()? - b.number()?
                                        };
                                        if !value.is_finite() {
                                            return Err(Error::Overflow);
                                        }
                                        Scalar::Float(value)
                                    }
                                }
                            }
                        };
                        record.fields.insert(name, new);
                    }
                    let (s, c) = self.prepare(record)?;
                    rows[0] = Work::new(s.record.clone());
                    pending_put = Some((s, c));
                }
                Stage::LinkAdd { target, cascade } => {
                    if rows.is_empty() {
                        continue;
                    }
                    let dependent = rows[0].record.as_ref().unwrap().actor;
                    let target = self.stored(*target)?.snapshot.record.actor;
                    self.snapshot(target, token)?;
                    if !self.links.contains(dependent, target)
                        && self.links.count() >= self.limits.rows_per_core * self.shards.len() * 4
                    {
                        return Err(Error::Budget);
                    }
                    pending_link = Some(Edge {
                        dependent,
                        target,
                        on_delete: if *cascade {
                            OnDelete::Cascade
                        } else {
                            OnDelete::Restrict
                        },
                    });
                }
                Stage::Project(items) => {
                    for w in &mut rows {
                        let mut out = Fields::new();
                        for i in items {
                            charge(&mut stats, 1, self.limits.query_work)?;
                            let value = w.field(&i.field)?;
                            scratch = scratch.saturating_add(i.alias.len() + value.bytes());
                            if scratch > self.limits.result_bytes.saturating_mul(4) {
                                return Err(Error::Budget);
                            }
                            out.insert(i.alias.to_owned(), value);
                        }
                        w.values = Some(out);
                    }
                }
                Stage::GroupBy(paths) => group_paths = Some(paths.as_slice()),
                Stage::Reduce(items) => {
                    rows = aggregate(
                        rows,
                        group_paths.take(),
                        items,
                        token.tenant_incarnation(),
                        &self.limits,
                        &mut stats,
                    )?;
                }
                Stage::Take(n) => rows.truncate(usize::try_from(*n).unwrap_or(usize::MAX)),
                Stage::SearchBm25 {
                    field,
                    query,
                    top_k,
                } => {
                    let field = if field.first() == Some(&"state") {
                        field[1..].join(".")
                    } else {
                        field.join(".")
                    };
                    let query = ciagomem_ql::unescape(query).map_err(|_| Error::Parse)?;
                    let terms = index::tokens(&query);
                    charge(
                        &mut stats,
                        rows.len().saturating_mul(terms.len().max(1)),
                        self.limits.query_work,
                    )?;
                    let mut scores = BTreeMap::new();
                    for (core, shard) in self.shards.iter().enumerate() {
                        let bits: RoaringBitmap = rows
                            .iter()
                            .filter(|w| self.owner_core(w.actor()) == core)
                            .map(|w| self.stored(w.actor()).map(|s| s.slot))
                            .collect::<Result<Vec<_>>>()?
                            .into_iter()
                            .collect();
                        // Upcast projection and search-index versions must agree.
                        if self.schemas.latest() != 1 {
                            return Err(Error::Unsupported);
                        }
                        for (slot, score) in shard
                            .index
                            .get(&token.tenant_incarnation())
                            .map(|index| index.bm25(&field, &query, &bits))
                            .unwrap_or_default()
                        {
                            scores.insert((core, slot), score);
                        }
                    }
                    let mut scored = Vec::new();
                    for w in rows {
                        let core = self.owner_core(w.actor());
                        let slot = self.stored(w.actor())?.slot;
                        if let Some(score) = scores.get(&(core, slot)) {
                            scored.push((w, *score));
                        }
                    }
                    scored.sort_by(|(a, x), (b, y)| {
                        y.total_cmp(x).then_with(|| a.actor().cmp(&b.actor()))
                    });
                    scored.truncate(*top_k as usize);
                    rows = scored
                        .into_iter()
                        .enumerate()
                        .map(|(rank, (mut w, _))| {
                            w.score = Some(w.score.unwrap_or(0.0) + 1.0 / (61.0 + rank as f32));
                            w
                        })
                        .collect();
                    sort_rank(&mut rows);
                }
                Stage::Similarity {
                    vector,
                    metric,
                    threshold,
                } => {
                    if let Some(cap) = options.approximate_candidates {
                        if *metric != Metric::Cosine {
                            return Err(Error::Unsupported);
                        }
                        let query = Sq8::quantize(vector)?;
                        let mut scored = Vec::new();
                        for w in rows {
                            charge(&mut stats, vector.len(), self.limits.query_work)?;
                            let r = w.record.as_ref().ok_or(Error::Unsupported)?;
                            if r.embedding.len() != vector.len() {
                                continue;
                            }
                            let stored = self.stored(r.actor.id)?;
                            let core = self.owner_core(r.actor.id);
                            let index = self.shards[core]
                                .index
                                .get(&r.tenant)
                                .ok_or(Error::MissingActor)?;
                            let code =
                                index.vectors.get(&stored.slot).ok_or(Error::InvalidValue)?;
                            let score = code.cosine(&query)?;
                            scored.push((w, score));
                        }
                        scored.sort_by(|(a, x), (b, y)| {
                            y.total_cmp(x).then_with(|| a.actor().cmp(&b.actor()))
                        });
                        scored.truncate(cap);
                        rows = scored.into_iter().map(|(w, _)| w).collect();
                        stats.approximate = true;
                    }
                    let mut scored = Vec::new();
                    for w in rows {
                        let r = w.record.as_ref().ok_or(Error::Unsupported)?;
                        if r.embedding.len() != vector.len() {
                            continue;
                        }
                        charge(&mut stats, vector.len(), self.limits.query_work)?;
                        let value = similarity(&r.embedding, vector, *metric)?;
                        let accept = if *metric == Metric::L2 {
                            value <= *threshold as f64
                        } else {
                            value >= *threshold as f64
                        };
                        if accept {
                            scored.push((w, if *metric == Metric::L2 { -value } else { value }));
                        }
                    }
                    scored.sort_by(|(a, x), (b, y)| {
                        y.total_cmp(x).then_with(|| a.actor().cmp(&b.actor()))
                    });
                    rows = scored
                        .into_iter()
                        .enumerate()
                        .map(|(rank, (mut w, _))| {
                            w.score = Some(w.score.unwrap_or(0.0) + 1.0 / (61.0 + rank as f32));
                            w
                        })
                        .collect();
                    sort_rank(&mut rows);
                }
                _ => return Err(Error::Unsupported),
            }
        }
        let mut out = Vec::new();
        for w in rows {
            let values = w
                .values
                .as_ref()
                .or_else(|| w.record.as_ref().map(|r| &r.fields))
                .ok_or(Error::Unsupported)?;
            let bytes = values
                .iter()
                .map(|(k, v)| k.len() + v.bytes())
                .sum::<usize>()
                + w.joins
                    .iter()
                    .map(|(a, r)| {
                        a.len()
                            + r.fields
                                .iter()
                                .map(|(k, v)| k.len() + v.bytes())
                                .sum::<usize>()
                    })
                    .sum::<usize>()
                + 128;
            stats.returned_bytes = stats
                .returned_bytes
                .checked_add(bytes)
                .ok_or(Error::Budget)?;
            if stats.returned_bytes > self.limits.result_bytes {
                return Err(Error::Budget);
            }
            let values = values.clone();
            let joins = w
                .joins
                .into_iter()
                .map(|(alias, r)| (alias, r.fields.clone()))
                .collect();
            out.push(ResultRow {
                actor: w.record.map(|r| r.actor),
                values,
                joins,
                label: w.label,
                score: w.score,
            });
        }
        // All downstream computation and result budgets succeed before publication.
        if let Some((s, c)) = pending_put {
            self.commit(vec![Change::Put(s.clone())])?;
            self.install(s, c);
        }
        if let Some(e) = pending_link {
            self.commit(vec![Change::Link(e)])?;
            self.links.add(e);
        }
        Ok(QueryResult { rows: out, stats })
    }
}
fn sort_rank(rows: &mut [Work]) {
    rows.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .total_cmp(&a.score.unwrap_or(0.0))
            .then_with(|| a.actor().cmp(&b.actor()))
    });
}
fn evaluate(e: &BoolExpr<'_>, row: &Work) -> Result<bool> {
    match e {
        BoolExpr::And(es) => {
            for e in es {
                if !evaluate(e, row)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        BoolExpr::Or(es) => {
            for e in es {
                if evaluate(e, row)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        BoolExpr::Pred(p) => {
            let a = row.field(&p.field)?;
            if p.op == CompOp::In {
                let Value::Array(vs) = &p.rhs else {
                    return Err(Error::Type);
                };
                for v in vs {
                    let Value::Lit(b) = v else {
                        return Err(Error::Type);
                    };
                    if a.compare(&literal(*b)?)? == Some(Ordering::Equal) {
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
            let Value::Lit(b) = p.rhs else {
                return Err(Error::Type);
            };
            let b = literal(b)?;
            if p.op == CompOp::Contains {
                return match (a, b) {
                    (Scalar::Text(a), Scalar::Text(b)) => Ok(a.contains(&b)),
                    _ => Err(Error::Type),
                };
            }
            let cmp = a.compare(&b)?;
            Ok(match p.op {
                CompOp::Eq => cmp == Some(Ordering::Equal),
                CompOp::Ne => cmp != Some(Ordering::Equal),
                CompOp::Gt => cmp == Some(Ordering::Greater),
                CompOp::Lt => cmp == Some(Ordering::Less),
                CompOp::Ge => matches!(cmp, Some(Ordering::Greater | Ordering::Equal)),
                CompOp::Le => matches!(cmp, Some(Ordering::Less | Ordering::Equal)),
                _ => return Err(Error::Unsupported),
            })
        }
    }
}
fn similarity(a: &[f32], b: &[f32], metric: Metric) -> Result<f64> {
    if a.len() != b.len() || a.is_empty() {
        return Err(Error::Type);
    }
    let mut dot = 0.0;
    let mut aa = 0.0;
    let mut bb = 0.0;
    let mut dist = 0.0;
    for (a, b) in a.iter().zip(b) {
        let (a, b) = (*a as f64, *b as f64);
        dot += a * b;
        aa += a * a;
        bb += b * b;
        dist += (a - b).powi(2);
    }
    Ok(match metric {
        Metric::Dot => dot,
        Metric::L2 => dist.sqrt(),
        Metric::Cosine => {
            if aa == 0.0 || bb == 0.0 {
                0.0
            } else {
                (dot / (aa * bb).sqrt()).clamp(-1.0, 1.0)
            }
        }
    })
}
fn aggregate(
    rows: Vec<Work>,
    paths: Option<&[FieldPath<'_>]>,
    items: &[AggItem<'_>],
    tenant: u128,
    limits: &Limits,
    stats: &mut QueryStats,
) -> Result<Vec<Work>> {
    let mut groups: BTreeMap<Vec<Key>, Vec<Work>> = BTreeMap::new();
    let mut group_bytes = 0usize;
    for row in rows {
        charge(stats, 1, limits.query_work)?;
        let key = paths
            .unwrap_or(&[])
            .iter()
            .map(|p| row.field(p)?.key())
            .collect::<Result<Vec<_>>>()?;
        if !groups.contains_key(&key) {
            if groups.len() >= limits.groups {
                return Err(Error::Budget);
            }
            group_bytes = group_bytes.saturating_add(
                128 + key
                    .iter()
                    .map(|k| match k {
                        Key::Text(s) => s.len() + 32,
                        _ => 32,
                    })
                    .sum::<usize>(),
            );
            if group_bytes > limits.result_bytes.saturating_mul(4) {
                return Err(Error::Budget);
            }
        }
        groups.entry(key).or_default().push(row);
    }
    if paths.is_none() && groups.is_empty() {
        groups.insert(Vec::new(), Vec::new());
    }
    let mut out = Vec::new();
    for (_, rows) in groups {
        let mut values = Fields::new();
        let mut label = rows
            .first()
            .map(|w| w.label.clone())
            .unwrap_or(Label::external(0, vec![tenant]).unwrap());
        for row in &rows {
            label = label.join(&row.label);
        }
        if let (Some(paths), Some(row)) = (paths, rows.first()) {
            for p in paths {
                values.insert(p.path.join("."), row.field(p)?);
            }
        }
        for item in items {
            charge(stats, rows.len(), limits.query_work)?;
            let mut vals = Vec::new();
            let mut bytes = 0usize;
            for r in &rows {
                let value = item.arg.as_ref().map_or(Ok(Scalar::Null), |p| r.field(p))?;
                bytes = bytes.saturating_add(value.bytes());
                if bytes > limits.result_bytes.saturating_mul(4) {
                    return Err(Error::Budget);
                }
                vals.push(value);
            }

            let present: Vec<_> = vals.iter().filter(|v| !matches!(v, Scalar::Null)).collect();
            let value = match item.func {
                AggFunc::Count => Scalar::Int(if item.arg.is_none() {
                    rows.len()
                } else {
                    present.len()
                } as i64),
                AggFunc::CountDistinct => Scalar::Int(
                    present
                        .iter()
                        .map(|v| v.key())
                        .collect::<Result<BTreeSet<_>>>()?
                        .len() as i64,
                ),
                _ if present.is_empty() => Scalar::Null,
                AggFunc::Sum if present.iter().all(|v| matches!(v, Scalar::Int(_))) => {
                    let total = present.iter().try_fold(0i128, |a, v| {
                        if let Scalar::Int(n) = v {
                            a.checked_add(*n as i128).ok_or(Error::Overflow)
                        } else {
                            unreachable!()
                        }
                    })?;
                    Scalar::Int(total.try_into().map_err(|_| Error::Overflow)?)
                }
                AggFunc::Min | AggFunc::Max => {
                    let mut best = (*present[0]).clone();
                    for v in &present[1..] {
                        let cmp = v.compare(&best)?;
                        if cmp
                            == Some(if item.func == AggFunc::Min {
                                Ordering::Less
                            } else {
                                Ordering::Greater
                            })
                        {
                            best = (*v).clone();
                        }
                    }
                    best
                }
                _ => {
                    let mut ns = present
                        .iter()
                        .map(|v| v.number())
                        .collect::<Result<Vec<_>>>()?;
                    let value = match item.func {
                        AggFunc::Sum => ns.iter().sum(),
                        AggFunc::Avg => ns.iter().sum::<f64>() / ns.len() as f64,
                        AggFunc::P50 | AggFunc::P95 | AggFunc::P99 => {
                            ns.sort_by(f64::total_cmp);
                            let percent = match item.func {
                                AggFunc::P50 => 50,
                                AggFunc::P95 => 95,
                                _ => 99,
                            };
                            ns[(percent * ns.len()).div_ceil(100) - 1]
                        }
                        _ => return Err(Error::Unsupported),
                    };
                    if !value.is_finite() {
                        return Err(Error::Overflow);
                    }
                    Scalar::Float(value)
                }
            };
            if values.insert(item.alias.to_owned(), value).is_some() {
                return Err(Error::Duplicate);
            }
        }
        out.push(Work {
            record: None,
            values: Some(values),
            joins: BTreeMap::new(),
            label,
            score: None,
        });
    }
    Ok(out)
}

struct DeleteJob {
    tenant: u128,
    cursor: u64,
    pending: std::collections::VecDeque<ActorRef>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteStatus {
    pub job: u64,
    pub cursor: u64,
    pub remaining: usize,
}
impl<J: Journal> Engine<J> {
    /// A fail-forward saga: tombstone the entire closure, then reclaim bounded chunks.
    /// It does not provide an all-at-once physical cascade or compensate by resurrecting data.
    pub fn begin_delete(
        &mut self,
        root: ActorRef,
        token: &VerifiedMacaroon,
    ) -> Result<DeleteStatus> {
        self.authorize(token)?;
        if !token.allows(OP_WRITE) {
            return Err(Error::Permission);
        }
        self.snapshot(root, token)?;
        let plan = self.links.plan_delete(root, self.limits.cascade_rows)?;
        if plan.len() > self.limits.cascade_rows.saturating_sub(self.deleting.len()) {
            return Err(Error::Budget);
        }
        for actor in &plan {
            self.snapshot(*actor, token)?;
        }
        let job = self.sequence.checked_add(1).ok_or(Error::Overflow)?;
        self.commit(vec![Change::BeginDelete {
            job,
            plan: plan.clone(),
        }])?;
        for actor in &plan {
            self.deleting.insert(*actor);
        }
        let remaining = plan.len();
        self.jobs.insert(
            job,
            DeleteJob {
                tenant: token.tenant_incarnation(),
                cursor: 0,
                pending: plan.into(),
            },
        );
        Ok(DeleteStatus {
            job,
            cursor: 0,
            remaining,
        })
    }
    pub fn delete_status(&self, job: u64, token: &VerifiedMacaroon) -> Result<DeleteStatus> {
        self.authorize(token)?;
        let j = self.jobs.get(&job).ok_or(Error::MissingActor)?;
        if j.tenant != token.tenant_incarnation() || !token.allows(OP_WRITE) {
            return Err(Error::Permission);
        }
        Ok(DeleteStatus {
            job,
            cursor: j.cursor,
            remaining: j.pending.len(),
        })
    }
    pub fn advance_delete(
        &mut self,
        job: u64,
        cursor: u64,
        max: usize,
        token: &VerifiedMacaroon,
    ) -> Result<DeleteStatus> {
        self.authorize(token)?;
        if max == 0 {
            return Err(Error::InvalidValue);
        }
        let status = self.delete_status(job, token)?;
        if cursor != status.cursor {
            return Err(Error::StaleReference);
        }
        let plan: Vec<_> = self.jobs[&job]
            .pending
            .iter()
            .take(max.min(self.limits.cascade_rows))
            .copied()
            .collect();
        let next = cursor.checked_add(1).ok_or(Error::Overflow)?;
        let mut changes: Vec<_> = plan.iter().copied().map(Change::Delete).collect();
        changes.push(Change::DeleteProgress { job, cursor: next });
        self.commit(changes)?;
        for actor in &plan {
            let core = self.owner_core(actor.id);
            let shard = &mut self.shards[core];
            let old = shard
                .rows
                .remove(&actor.id)
                .expect("tombstoned actor remains allocated");
            shard
                .index
                .get_mut(&old.snapshot.record.tenant)
                .unwrap()
                .remove(old.slot, &old.snapshot.record);
            if shard.index[&old.snapshot.record.tenant].all.is_empty() {
                shard.index.remove(&old.snapshot.record.tenant);
            }
            shard.bytes -= old.charge;
            shard.by_slot.remove(&old.slot);
            shard.free.push(old.slot);
            self.links.remove_actor(*actor);
            self.deleting.remove(actor);
        }
        let j = self.jobs.get_mut(&job).unwrap();
        j.cursor = next;
        for _ in &plan {
            j.pending.pop_front();
        }
        let result = DeleteStatus {
            job,
            cursor: next,
            remaining: j.pending.len(),
        };
        if result.remaining == 0 {
            self.jobs.remove(&job);
        }
        Ok(result)
    }
}
