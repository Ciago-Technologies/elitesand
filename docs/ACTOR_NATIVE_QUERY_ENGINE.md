# CiagoMem actor-native query and relationship engine

This document specifies the five database capabilities in the CiagoMem ecosystem and records their implementation boundaries. The accompanying `ciagomem-engine` crate is an executable, owner-local reference engine. It composes typed CiaQL, verified sessions, per-tenant indexes, immutable snapshots, and journal-gated writes. It does not start a network server or pin operating-system threads.

## 1. Architecture and invariants

```text
CiaQL text
  → bounded lexer and typed AST
  → session / operation / actor policy
  → stage and consistency checks
  → per-tenant, per-core candidate selection
  → exact residual evaluation
  → relationship / search / aggregation stages
  → result budget check
  → journal batch for pending writes
  → owner-local state and index publication
```

An actor is identified by `(id: u64, incarnation: u128)`. Tenant incarnation is separate from actor incarnation. Both are retained in the FlatBuffers image. Reusing an actor ID does not make an old reference valid.

The reference partition is `actor_id % core_count`. Each shard owns its row directory, slot allocator, tenant index namespaces and accounting. Its hot structures have no global index lock. `Engine` coordinates these shards through exclusive mutable access in one process; they are **simulated owner cores**, not concurrently running reactor threads. A production dispatcher must move owner state/endpoints to pinned threads and preserve the same publication contracts.

`Snapshot` holds `Arc<Record>` and `Arc<[u8]>`. Copying its handle pins the immutable version; it does not copy the underlying actor image. This is same-process reference sharing, not raw pointer sharing across process boundaries. FlatBuffers borrowed getters avoid decoding their payloads. Query results, mutable field maps, schema upgrades and joins that materialize values can still allocate and copy.

Every execution calls the existing structural and authorization policy. Tenant indexes are separate, and text statistics use the current authorized candidate corpus. Operator scan permission does not grant reads of every tenant's payload. Dynamically resolved join targets are checked again at execution time. Result labels conservatively join all participating actor labels.

## 2. Joins: reference lookup and bounded read fan-out

### Direct relationship path

A field stores `Scalar::Ref(ActorRef { id, incarnation })`, not a raw pointer or an unversioned integer. Example:

```text
@1
join @actor(state.organization) as @org
where @org.state.name == "Ciago"
select {
  organization_name: @org.state.name,
  balance: state.wallet.balance
}
```

The parser represents JOIN targets and alias-qualified paths as typed nodes. An alias must be declared before use, cannot be declared twice, and cannot be a mutation target. A `$ref` vault capability and an `ActorRef` relationship are different types with different purposes.

Execution resolves the base field, verifies actor and tenant incarnations, checks session read authority and the target label, and pins an immutable target snapshot. A missing or stale reference returns an explicit error rather than silently joining a newly created actor that reused the ID. The reference executor fails the query on a missing target; it does not implement SQL LEFT/RIGHT/FULL join syntax.

### Cross-core protocol

`join::Request` carries a unique ticket, authenticated tenant context, full target reference and deadline. `join::Fanout` tracks at most a configured number of outstanding targets. Replies must match the ticket and actor reference; a successful snapshot must match the tenant and reader permission. Wrong-target responses cannot consume the valid request. Expiration releases pending state; late and duplicate replies fail.

The integration suite sends these requests and replies over the existing real `Mesh<Request, N>` and `Mesh<Reply, N>` Lamport SPSC lanes and verifies that the returned record is the same `Arc` as the owner snapshot. Endpoints remain single-producer/single-consumer. A full lane returns the unsent message; the dispatcher must retry within a bounded budget or reject admission. It must never spin until a peer drains.

The direct executor currently resolves cross-shard reads synchronously in-process. The ticket protocol and actual mesh transport are tested together, but a continuously running asynchronous reactor dispatcher is not supplied. A reference-engine call holds exclusive engine access; mesh replies independently provide per-actor committed snapshots, not a common distributed snapshot epoch. An integrated deployment needs an epoch or version-validation rule for stronger join consistency.

### General equijoin path

`join::equi_join` handles relationships without direct references. It builds a bounded ordered key map for one input, probes the other, preserves duplicate matches, skips missing/null keys, verifies every input tenant/label, joins provenance, and returns pinned snapshots. Input work, build-key bytes, result rows and descriptor bytes are bounded.

Its keys use explicit scalar types, like grouping. It does not promise SQL implicit casts. This API is programmatic; the CiaQL JOIN syntax currently selects the actor-reference path. Arbitrary predicate joins, distributed repartition joins and unbounded Cartesian products are not implemented. They require explicit work limits and spill/admission semantics rather than an assumption that mesh messaging makes them cheap.

Read-only fan-out is **not a saga**. Sagas describe multi-step writes and compensation or fail-forward cleanup, not the consistency of a read join.

## 3. Referential integrity, RESTRICT and cascades

An explicit edge records `dependent ActorRef → target ActorRef` and an `OnDelete` policy. `Links` stores incoming edges for efficient delete planning and outgoing edges for cleanup. Both endpoints must exist in the same authorized tenant at link creation. The total edge population is capped. A plain reference field does not automatically register a constraint; applications must create the edge.

```text
@2 link add(target: @1, on_delete: RESTRICT)
@3 link add(target: @1, on_delete: CASCADE)
```

Deletion walks incoming CASCADE edges with a visited set. Cycles terminate. It rejects an oversized closure and checks RESTRICT dependencies outside the proposed closure. Validation completes before any deletion record is committed.

### Strict owner-coordinated deletion

`Engine::delete` journals one batch containing the complete bounded closure, then removes rows, bitmap membership, text postings, vector entries and links. A journal error leaves the previous visible state unchanged. This provides atomic visibility within the reference engine's exclusive owner operation. Durable, cross-reactor atomicity still requires the integrated Calvin/WAL protocol; an in-memory batch is not proof of distributed ACID.

### Explicit asynchronous cleanup

`begin_delete` journals the cascade plan and marks every actor in its closure as deleting. Such actors disappear from scans and cannot accept new links or mutations. `advance_delete(job, expected_cursor, max_rows, session)` journals a bounded chunk plus cursor progress before physically reclaiming it. Repeated/stale cursor calls are rejected rather than deleting another chunk accidentally. The caller can inspect the authorized status and resume with the current cursor. Job/plan memory shares a fixed closure budget.

This is a **fail-forward deletion saga**. It never compensates by resurrecting deleted data. Logical deletion is established before incremental reclamation; physical cleanup is eventual, not instant. Journal adapters and restart recovery must persist/replay the plan, tombstones and progress together. The supplied MemoryJournal records them in process but does not recover them after process loss.

These mechanisms move integrity checks to actor-owned metadata. They do not eliminate integrity-checking overhead, and unregistered asynchronous hooks are not a substitute for constraints.

## 4. Verified FlatBuffers and lazy schema evolution

`schemas/actor.fbs` defines the physical actor image with explicit identity, tenant, application schema version, scalar fields, provenance/readers and an optional embedding. Code is generated by upstream `flatc 25.2.10`; generated bindings are committed so normal builds need no compiler installation.

`image::encode` constructs an immutable buffer. `image::view` checks the identifier and verifies the root before exposing borrowed accessors; it also checks paired reader-vector lengths. Only this verified path is used by the handwritten code. The private generated module contains upstream unsafe implementation details and is isolated from handwritten safe execution code. Verification does not authorize a buffer or authenticate its contents.

Application schemas advance through an ordered registry of `fn(&Fields) -> Result<Fields>` transitions. The reference registry describes one schema family per Engine. Registering version N→N+1 requires N to be current. Every transition must be deterministic and total over valid input, and bounded by the actor budget. Those are application contracts to test; Rust function pointers alone do not prove purity.

`Engine::projected` returns a current-schema view while the stored version remains untouched. Query execution does the same for old-schema actors. `Engine::upgrade` is an explicit owner write: derive the new fields, preserve identity/labels/embedding, validate and encode the result, journal it, then publish it and refresh indexes. No reader performs an in-place rewrite or changes a seqlock sequence.

A projection-only read can allocate for the transformed map; it is not a universally zero-copy operation. Avoid eager fleet-wide rewrites. New fields may receive defaults through projections. Physical FlatBuffers evolution must follow upstream rules: append fields or use explicit stable IDs, deprecate rather than remove, and check compatibility with `flatc --conform` in a schema-release pipeline. Do not change field types/defaults casually.

The P0 A/B checkpoint format has not been replaced by this image format. Versioned WAL/checkpoint records, durable schema registration, multi-family catalogs and restart replay must be integrated before declaring live production schema migration complete. Indexed search is conservatively rejected after advancing the global schema registry until a version-consistent index migration path exists; it cannot silently use stale postings against projected data.

## 5. Text search, SQ8 vectors and hybrid ranking

### Text index

Each tenant/core namespace has field-specific inverted postings `(field, term) → (slot, term_frequency)` and document lengths. The initial tokenizer splits on non-alphanumeric characters and lowercases Unicode text. It does not provide stemming, stopwords, phrase/proximity search or language-specific analyzers.

Insertion and replacement update current postings; deletion removes membership and empty containers. The current implementation uses mutable owner-local postings, not an append-only segment/compaction engine. A future log-structured index must carry actor incarnations/versions and tombstone stale postings.

BM25 uses k1=1.2 and b=0.75. Corpus size, mean document length and document frequency are calculated from the authorized candidate set at that stage. This gives explicit filtered-corpus ranking and avoids cross-tenant statistics. Local scores are combined as candidates; a production global-BM25 mode needs authorized global statistics. Deterministic tie-breaking uses actor IDs.

### SQ8 and SIMD

Each vector retains its full-precision source plus symmetric signed 8-bit codes with scale `max(abs(x))/127`, protected against zero/subnormal scale. Values/dimensions are validated at ingestion. Eight i32 lanes accumulate quantized dot products through `wide::i32x8`, with a scalar tail. The 4,096-dimensional bound keeps integer accumulation within i32 range. SIMD instruction selection depends on compiler target capabilities and may use portable fallback paths.

Exact execution is the default: cosine, dot and Euclidean distance use retained full-precision values, and thresholds are checked after scoring. L2 accepts distance ≤ threshold; cosine/dot accept score ≥ threshold. Zero-vector cosine is defined as zero.

Explicit `Options { approximate_candidates: Some(K), ..Default::default() }` uses SQ8 cosine to shortlist K candidates and reranks them with full precision. It reports `stats.approximate=true`. Approximate shortlisting can lose recall: it is never presented as an exact predicate. The current shortlist enumerates candidates; it is not an ANN graph index. Keeping both representations costs more than codes alone and is included in accounting.

### One CiaQL pipeline

```text
scan actors[*]
where state.active == true
search(field: state.body, query: "agent memory", top_k: 50)
similarity(vector: [0.12, -0.88, 0.44], metric: COSINE, threshold: 0.8)
select {summary: state.body}
take 10
```

The executor currently resolves `state` fields and `meta.schema_version`; other parsed metadata/frame/episode namespaces are not silently interpreted. Unsupported execution paths return errors.

Stage order is semantic. SEARCH's top_k can exclude a row before SIMILARITY sees it. The planner does not move filters across top-k stages. Hybrid stages fuse rank contributions using reciprocal rank `1/(60 + rank)` with ranks starting at one, avoiding the assumption that raw BM25 and cosine scores share a scale. Deterministic ties use actor ID. No vector model, embedding service or embedding generation is supplied.

## 6. Scans, bitmaps, zone maps and analytics

The initial planner uses bounded, explicit rules rather than an expensive search over arbitrary join plans. This is a scoped rule planner, not a claim that optimizers have no value. `Options::disable_indexes` runs the same semantics through scalar selection for differential tests and benchmarks.

A leading FILTER may be pushed into per-tenant indexes before snapshot materialization. Equality uses upstream Roaring bitmaps. AND intersects available safe candidate sets and keeps exact residual checks. OR is pushed down only when every branch has a complete safe candidate set. Missing/null fields and unsupported comparisons fall back to scalar evaluation. Mixed integer/float comparisons avoid rounding away i64 identity above 2^53.

Numeric columns maintain min/max zones over 64 slot positions. Updates/deletions refresh the affected zone, and slot reuse cannot inherit a stale bound. Nonmatching zones are skipped. Surviving integer values are tested in four-lane SIMD batches through `wide::i64x4`, including tail masks. Exact residual evaluation still runs on candidates. Mixed-float ranges use scalar fallback.

```text
scan actors[*]
where state.subscription == "enterprise" and state.spend >= 5000
group by (state.region)
aggregate {
  users: count(),
  revenue: sum(state.spend),
  distinct_orgs: count_distinct(state.org_id),
  p95_spend: p95(state.spend)
}
take 20
```

The executor implements COUNT, COUNT_DISTINCT, SUM, AVG, MIN, MAX and exact nearest-rank P50/P95/P99. Integer SUM accumulates in i128 and rejects i64 result overflow; it never silently wraps. AVG and floating aggregates reject unsafe conversions/nonfinite outputs. Distinct/group keys preserve explicit scalar types. Nulls are excluded by aggregates except COUNT(), which counts rows. Ungrouped aggregation over no rows produces COUNT=0 and null numeric results; grouped aggregation over no rows produces no groups.

CiaQL is not SQL: missing fields evaluate as Null; Null==Null is true; incomparable scalar types have no ordering. These semantics are explicit and tested rather than advertised as SQL compatibility. Group output can be projected as `state.<aggregate_alias>`.

Current execution batches candidates and bounds scratch use, result bytes, groups, joins, vectors, work units and text. It does not spill to disk or implement an unbounded streaming hash aggregate. It rejects work it cannot fit. SIMD/bitmap metadata and source maps are owner-local; no generic table lock is introduced.

Production parallel analytics should send bounded subplans to owners, return partial COUNT/SUM/MIN/MAX states and `(sum,count)` for AVG, and merge them at the coordinator. Distinct and quantiles need bounded exact state or explicitly approximate sketches. The current reference executor processes its shards on one thread; this protocol is design scope, not implemented parallel speedup.

## 7. Commit and recovery boundary

`Journal::commit(Batch)` receives validated Put/Delete/Link/BeginDelete/DeleteProgress records. A production adapter must durably append a complete recoverable batch before returning success. Queries finish downstream evaluation and output-budget checks before committing pending mutations. An error or refused journal batch leaves prior published actor/index state unchanged.

`MemoryJournal` is deliberately non-durable and bounded. It makes these transitions observable in tests; it is not a WAL adapter, crash replay engine or ACID certification. Integrating the P0 group-commit log requires a versioned batch encoding, prepare/decision boundaries, completion-driven publication, metadata recovery and explicit durable client acknowledgements. Returning success from the reference engine means its configured journal accepted the batch, not that an NVMe or replica persisted it.

## 8. Memory and performance contracts

`Limits` bounds actors per core, estimated owner bytes, query work, result bytes, groups, joins, fields, text, vector dimensions and cascade closure size. Record images, typed shadows, index estimates and retained journal images all consume memory; they are not free because a payload can be borrowed.

Accounted bytes are conservative logical estimates, not an allocator-enforced physical RAM quota. Pinned snapshots retained by external callers, BTree allocator details, VMAs, kernel buffers and the server session budget require separate accounting. A production engine must charge outstanding snapshot lifetimes to the request/session and integrate these limits with the 16 GiB node budget.

Index memory trades against the resident working set. Index only fields justified by workload selectivity in a production catalog; the reference indexes all scalar/text fields for functional coverage. Do not extrapolate the example dataset directly to one million users. The reference mesh demonstration does not copy actor images, but projection, tokenization, image construction, index maintenance and schema conversion still cost work.

Run the reproducible local comparison:

```sh
cargo run --release --offline -p ciagomem-engine --example benchmark
```

It reports snapshot-handle time, accounted bytes and identical selective queries with and without bitmap pushdown over 10,000 records and four simulated shards. The source, dataset and query are committed. It does not compare against PostgreSQL, MySQL, Redis, a vector database, or another AI-memory system. The host is not an isolated benchmark runner; outputs are observations, not release guarantees.

## 9. Open-source components and provenance

| Component           | Use                                                      | Upstream license          |
| ------------------- | -------------------------------------------------------- | ------------------------- |
| roaring 0.10.12     | Compressed equality/candidate sets                       | MIT OR Apache-2.0         |
| flatbuffers 25.2.10 | Verified immutable actor images and generated bindings   | Apache-2.0                |
| wide 0.7.33         | Safe SIMD numeric comparisons and quantized dot products | Zlib OR Apache-2.0 OR MIT |

Transitive dependencies and upstream license files are retained unmodified in `vendor/`. Archive SHA256 values are recorded in `vendor/archive-sha256.json`. The versions are compatible pinned inputs, not a claim that they are the newest available or independently security-audited. Dependency/advisory monitoring and update policy remain release responsibilities.

Primary references:

- [Roaring Rust implementation](https://github.com/RoaringBitmap/roaring-rs).
- [FlatBuffers Rust usage](https://flatbuffers.dev/languages/rust/), [schema evolution rules](https://flatbuffers.dev/evolution/).
- [wide 0.7.33 SIMD API](https://docs.rs/wide/0.7.33/wide/).

## 10. Verification and remaining integration

`tests/ecosystem.rs` covers joins across actual SPSC lanes, snapshot ownership, foreign/stale references, alias policy, strict and chunked cascades, cycles and retries, journal failure, schema projection/publication, borrowed FlatBuffers access, index/scalar equivalence including numeric limits, zone refresh and slot reuse, text update/delete, hybrid exact/approximate modes, SIMD tails, grouping, and admission failures.

Still required for a deployed database: a pinned asynchronous reactor dispatcher; durable WAL encoding/replay for all new records; persistent schema/index/link catalogs; multi-reactor transaction epochs; cold-I/O suspension; index reindexing across schemas; authorized request transport; complete memory accounting; client SDK/server interfaces; cross-system workload benchmarks; and hardware/failure qualification. Those gaps do not remove the five features from product scope, but they prevent representing this component milestone as a completed production system.

The mesh consumer refreshes a cached tail before clearing pending work after a new doorbell; a deterministic regression guards against stranding the final published batch.
