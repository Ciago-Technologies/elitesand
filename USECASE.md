# Production Use Cases & Architecture Blueprints

> **Single-node service update:** Linux TLS1.3/mTLS reactor hosting and durable client retries are implemented in the fixed-Record profile. See [specification20](MASTER_SPECIFICATION.md#20-implemented-single-node-tls-reactor-and-durable-client-retries) and [source deployment](INSTALL.md#run-the-current-single-node-service). BFBS families, Go/TypeScript SDKs and the distributed runtime remain target work; hardware qualification remains required.

> **Implementation status:** This guide includes target architecture and acceptance API examples. Current runnable integration is the [embedded Rust quickstart](ciagomem-engine/examples/quickstart.rs), using non-durable MemoryJournal. The fixed-Record Linux server and source Rust client now provide durable integration (§20). Runtime BFBS families, general arrays, Go/TypeScript SDKs and distributed services remain milestones in specification 14.11. Section 17 is the complete target application/operations contract; target examples are not runnable SDK releases or measured performance guarantees.


Ciagomem — SLAE is designed to replace fragmented multi-database architectures (such as running PostgreSQL + Redis + Pinecone + Kafka simultaneously) with a single, unified actor data plane.

Below are five target workload blueprints showing how to model schemas, configure durability, and execute CiaQL pipelines for real-world workloads.

---

## 1. Stateful AI Agents & Cognitive RAG Memory

### The Problem
Autonomous AI agents require three distinct types of state on every turn:
1. **Structured State:** Token budgets, tool permissions, and user preferences (traditionally in Postgres/Redis).
2. **Episodic Transcripts:** Full-text keyword recall over past conversations (traditionally in Elasticsearch).
3. **Semantic Memory:** High-dimensional vector embeddings for similarity recall (traditionally in Pinecone/Milvus).

Additional service hops require workload measurement. Prompt-injection risk comes from how untrusted content is used; a database boundary alone does not prevent it.

### The Ciagomem Solution
Each AI session or memory frame is stored as a single **Actor** containing scalar metadata, full-text fields, a quantized `SQ8` vector embedding, and a **CaMeL Provenance Label** (`External < AgentDerived < UserDirect < Policy`).

#### Schema (`agent_memory.fbs`)
```flatbuffers
namespace ciagomem.ai;

table MemoryEpisode {
  agent_id: string (id: 0);
  session_active: bool = true (id: 1);
  token_count: long = 0 (id: 2);
  role: string (id: 3);
  transcript: string (id: 4);
  recent_tools: [string] (id: 5);
  embedding: [float] (id: 6);
}

root_type MemoryEpisode;
file_identifier "MEMO";
```

#### Hybrid Search & Mutation Pipeline (TypeScript)
```typescript
// 1. Append a tool invocation to a bounded ring-buffer (keeps last 20 tools) and update token count
await client.mutate(
  memorySchema,
  "episode-9001",
  episodeRef,
  currentVersion,
  `mutate {
    state.token_count += 412,
    state.recent_tools.push_bounded("web_search", 20)
  }`
);

// Target example assumes a registered four-dimensional embedding.
// 2. Hybrid Recall: Filter active sessions -> BM25 text match -> SIMD Cosine similarity -> RRF fusion
const memories = await client.query(`
  scan family("agent_memories")
    |> filter (state.session_active == true and state.token_count < 8000)
    |> search(field: state.transcript, query: "kubernetes network policy", top_k: 50)
    |> similarity(vector: [0.041, -0.219, 0.884, 0.112], metric: COSINE, threshold: 0.78)
    |> select {
         role: state.role,
         summary: state.transcript,
         tokens: state.token_count
       }
    |> take 5
`);
```

---

## 2. High-Frequency FinTech Wallets & Real-Time Gaming State

### The Problem
Wallets, bidding engines and game servers can create contention on hot accounts. Test the actual workload, transaction requirements and contention pattern before choosing an implementation.

### The Ciagomem Solution
Every wallet or player state is isolated in a single-writer owner core. Updates use CiaQL's **Compact Binary Delta Tail** and optimistic version preconditions (`expected_document_version`), targeting qualified local durable group-commit latency (an objective, not an observed bound) without rewriting the base FlatBuffer.

#### Schema (`wallet.fbs`)
```flatbuffers
namespace ciagomem.fintech;

table WalletAccount {
  account_id: string (id: 0);
  status: string (id: 1);
  balance_cents: long = 0 (id: 2);
  tx_count: long = 0 (id: 3);
  recent_tx_ids: [string] (id: 4);
}

root_type WalletAccount;
file_identifier "WALT";
```

#### Atomic Balance Deduction with Optimistic Concurrency
```typescript
// Deduct 5,000 cents only if the account is ACTIVE and has sufficient funds
const txResult = await client.mutate(
  walletSchema,
  "acct-88412",
  walletRef,
  expectedVersion, // Fails with Status 5 (VersionConflict) if concurrent write occurred
  `where (state.status == "ACTIVE" and state.balance_cents >= 5000)
   mutate {
     state.balance_cents -= 5000,
     state.tx_count += 1,
     state.recent_tx_ids.push_bounded("tx_99481a", 50)
   }`
);
```

---

## 3. Multi-Tenant B2B SaaS & Referential Cascade Management

### The Problem
Enterprise SaaS platforms manage millions of registered tenants where only 10–20% are active at any given moment. Keeping all tenants in RAM is prohibitively expensive, while deleting an organization with thousands of linked projects and users often locks tables or leaves orphaned records.

### The Ciagomem Solution
* **Massive Working-Set Density:** Ciagomem's TinyLFU admission filter and segmented CLOCK eviction are planned toward **1,000,000 registered tenants** on a fully budgeted 16 GiB node, keeping the hot working set in 2 MiB HugePages while loading cold actors under separately measured NVMe latency and queue budgets.
* **Storage-Level Referential Integrity (`LinkGraph`):** Actors declare explicit `RESTRICT` or `CASCADE` dependency edges. Deletions can execute as a strict atomic batch or an asynchronous, fail-forward **Chunked Deletion Saga**.

#### Linking Actors & Executing a Deletion Saga
```text
# Prevent deleting Organization @1 while referencing Billing Invoice @2 exists
@2 link add(target: @1, on_delete: RESTRICT)

# Automatically cascade delete Workspace @3 when Organization @1 is deleted
@3 link add(target: @1, on_delete: CASCADE)
```

```typescript
// Start an asynchronous chunked deletion job (mode: 1) for a large organization tree
// Target recoverable bounded deletion plan; visibility fences and durable cleanup follow the job contract
const deleteJob = await client.delete(
  orgSchema,
  "org-enterprise-01",
  orgRef,
  orgVersion,
  { mode: 1 } // Returns DeleteJob { id, status }
);
```

---

## 4. Local Zero-Copy IPC for Co-Located Microservices & Edge Nodes

### The Problem
When microservices or AI worker processes run on the same physical Linux host or edge appliance as the database, serializing responses over TCP loopback (`127.0.0.1`) wastes CPU cycles and memory bandwidth.

### The Ciagomem Solution
By connecting over Ciagomem's local Unix Domain Socket (`SOCK_SEQPACKET` at `/run/ciagomem/db.sock`), co-located processes receive a kernel-sealed, read-only **`memfd` Export Window** (`SealedExportWindow`) via `SCM_RIGHTS`.

```rust
// Connect locally via Unix Domain Socket (SOCK_SEQPACKET)
let transport = Transport::UnixSeqpacket {
    path: "/run/ciagomem/db.sock".into(),
};

let mut client = Client::connect(transport, token, identity).await?;

// Reads <= 4,032 bytes can be projected directly into a sealed read-only memfd page (getter latency excludes projection construction, FD passing and request latency)
let doc = client.read(&schema, "user-42").await?;
let profile = flatbuffers::root::<UserProfile<'_>>(&doc.bytes)?;
```
## 5. Sub-Agent Memory Orchestration: The Zero-Token Waste Pattern

### Purpose

Keep repeated raw evidence out of the main agent's expensive context while retaining traceable, fresh memory. "Zero-Token Waste" describes the optimization goal: models still consume tokens when reading summaries, and worker/model/tool activity still has a cost. The pattern must be measured against answer quality and total cost, not a promised70–90% reduction.

**Available now:** a bounded Rust `MemoryBroker` over the reference Engine, with scoped worker leases, cited publication, source-revision invalidation and context byte budgets. **Planned integration:** namespace/family service isolation, named-family hybrid search, general delta arrays, network SDKs and restart-safe task recovery. See specification18 and the [research/threat assessment](docs/MEMORY_ORCHESTRATION_RESEARCH.md).

### Problem and division of labor

A main reasoning agent can repeatedly pay to ingest the same files, logs and transcripts. Its context grows even when the next decision needs only a few findings. Delegate evidence preparation once, then reuse a versioned summary while its sources, permissions and TTL remain valid.

The worker receives a concrete objective, allowed source references, output contract and resource budget. It reads only that evidence, uses deterministic filtering where sufficient, and optionally calls a lightweight model to summarize the remainder. It publishes structured memory directly through the scoped broker. Its completion message can contain just the opaque memory reference; raw logs never need to pass through the main model.

The main agent plans and reasons over the compact memory slice, merged with existing trusted policy/intent and useful prior notes. It normally avoids raw parsing; it may explicitly inspect cited evidence when the summary is uncertain, incomplete or insufficient for a high-impact decision. Delegation is an optimization, not a rule forbidding verification.

```text
+----------------------+   1. objective, allowed refs, budget   +------------------+
| Main / orchestrator  | ------------------------------------> | Worker sub-agent |
| reasoning + strategy |                                       | scoped extraction|
+----------^-----------+                                       +--------+---------+
           |                                                            |
           | 4. compact evidence + citations                            | 2. allowed raw
           |    (model tokens still apply)                              |    files/logs
+----------+-------------------+      3. structured publication  +--------v---------+
| Ciagomem context assembly   | <------------------------------ | worker summary   |
| broker + actor memory store |                                 | evidence, not    |
| budgets / freshness / label |                                 | policy authority |
+-----------------------------+                                 +------------------+
```

### Runnable handoff

Run the complete [Rust example](ciagomem-engine/examples/memory_orchestration.rs):

```sh
cargo run --release --offline -p ciagomem-engine --example memory_orchestration
```

It creates separate controller/worker sessions, ingests repetitive logs as an External source actor, extracts error lines in the worker, publishes a summary and returns a1-KiB-bounded main context. It calls no model service and reports bytes, not estimated tokens or billed savings.

The core handoff inside that configured host is:

```rust
let lease = broker.delegate(output_actor, &[source_actor], &worker, &controller)?;
let inputs = broker.read_sources(&lease, &worker)?; // Raw inputs stay on worker path.
// A bounded deterministic parser or sandboxed model prepares this untrusted String.
let memory = broker.publish(&lease, &summary, &worker)?;
let slice = broker.read_context(&[memory], 1024, &controller)?;
// Use slice.evidence_label() with the existing Var/data admission path; retain citations.
// Never insert it into system policy or automatically authorize a tool call.
```

There is no generic Engine access in worker broker APIs. The current broker owns the Engine and limits workers to the exact sources/output for their lease. Controller ingestion cannot overwrite reserved publications. Wrong tenant/session/audience/boot, expired token or lease, output collision, stale source and excessive input/output all reject. Same-content live-task publication retries return the existing reference only while non-stale and unexpired; stale/expired publications reject before cached success. Conflicting retries of otherwise valid publications reject. After a lost worker message, the controller can call result to reconcile before starting another job.

### Hybrid memory and target CiaQL workflow

Combine trusted fixed constraints and active intent, compact prior memory and just-in-time retrieval. The current broker accepts explicit memory references in the host's chosen order; automatic family-level retrieval remains target work. A future agent_memory family stores one searchable scalar summary per episode/task plus source manifest, extraction/model versions, task key, expiry and optional embedding. A separate scratch family holds bounded intermediate work; ordinary user collections are not a worker scratch directory.

Target candidate retrieval, assuming a registered four-dimensional embedding:

```text
scan family("agent_memory")
  |> filter (state.task_key == "auth-review-9001")
  |> search(field: state.summary, query: "auth module exception", top_k: 3)
  |> similarity(vector: [0.1, -0.2, 0.3, 0.4], metric: COSINE, threshold: 0.75)
  |> select {summary: state.summary}
  |> take 1
```

The host must authorize and freshness-check candidate refs before constructing the final bounded context. Search scalar summary text, not an entire `[string]` history vector. BM25/SQ8 ranking does not bypass labels, stale-source checks or final exact-score requirements. Cache identity includes task/source revisions, parser/prompt/model/embedding versions and policy scope; repeated raw work can be skipped only when those match.

In the target document path, scalar deltas and `push_bounded` maintain limited history without logging a full extent on each append. They still require typed validation, CAS versions, compaction and WAL/result-ledger integration. The current Rust broker publishes complete fixed-format Record images; it does not implement compact delta tails or general array execution. Never interpolate untrusted model output into CiaQL text; use validated typed document writes or a correctly escaped, schema-checked command encoder when that SDK is delivered.

### Security and reliability contract

- **Taint survives summarization.** Extracting an untrusted log does not make it AgentDerived. Existing derivation preserves minimum integrity, source union and reader intersection. Signed Fact/Policy admission and sensitive tool permits remain separate checks. Labels constrain authority when the actual host/tool path enforces them; they do not prove factual accuracy or eliminate every prompt injection.
- **Scope is explicit.** The implemented broker binds tenant, controller/worker session and source/output refs. Namespace/family caveats and remote transport are target services. A worker gets neither issuer keys nor direct database/filesystem/network authority. Verified token inputs must come from the trusted host verifier; this library does not isolate arbitrary native Rust plugins.
- **Freshness is revision-bound.** Source updates invalidate descendant summaries. Parent-summary expiry limits descendants. External file changes require a trusted adapter to report their new revision; the engine does not watch arbitrary paths. Preserve content hashes and cite source byte/line ranges in the target ingestion manifest.
- **Budgets include overhead.** Defaults bound64 tasks,16 sources,1 MiB source-image input, one source fetch,4 KiB summaries and8 KiB context. Context framing/citations count against the byte cap. Omitted items are reported; no silent UTF-8 truncation. Model tokens, worker calls and total tenant/process memory need additional trusted host budgets.
- **Retries are not restart recovery.** The current task table/cache is volatile and MemoryJournal is non-durable. Journal failure leaves publication unacknowledged; restart invalidates old broker refs. Target durable state, result ledger and recovery must be completed before advertising crash-safe orchestration.
- **Markdown is data.** A summary may contain Markdown text in an actor field. Writing a Markdown file is an optional trusted export adapter with a confined destination; it never grants the worker arbitrary writes to project instructions or policy files.

### Measure the outcome

Record main and worker input/output tokens, cached-token billing, provider/tool/embedding costs, retries, stale-cache misses, omitted evidence, grounding quality and end-to-end latency. Include deterministic preprocessing and direct-main baselines. Shared-memory/FlatBuffers efficiency reduces application transport work; it does not bypass model tokenization or imply sub-microsecond agent turns. If a worker adds cost without sufficient reuse or quality, use the direct/preprocessed path instead.

## 6. General application CRUD and telemetry

Users, sessions, orders, projects, sensor readings and other custom families follow the same schema/CRUD model; gaming and wallets are examples, not a specialization boundary. Applications choose keys, fields, relationships, expiry/retention rules and business invariants. Large blobs and unbounded event histories need an explicit external/chunking design beyond a single 64 KiB extent. The target feature/qualification checklist is specification 17.8.

## Target cross-workload scaling and isolation

The five blueprints share the target Hub-and-Spoke design in specification15.13–15.18. Gateways multiplex authenticated logical sessions and route through epoch-fenced placement; owner reactors handle bounded point state while reserved Query Workers handle heavy retrieval/analytics. Workers use immutable snapshots, cannot directly mutate actors and have CPU/pin/memory/I/O/output limits. Logical isolation does not eliminate shared hardware contention; measure point-read latency during search and aggregation load.

Hot session/account metadata remains in4–64 KiB actors. Long histories, telemetry, columnar data and vector structures use planned immutable NVMe/object segments referenced through durable manifests. Immutable hot publication may improve heavily read shared actors, subject to reclamation, identity/permission checks and charged extra images. It does not make Ciagomem Redis-compatible or turn every query into a200-ns operation.

One million active users requires a defined activity interval and request rate. One million outstanding requests is a cluster qualification objective with bounded per-node acceptance; gateways and query nodes add resources beyond each16-GiB storage node. Financial/game workloads remain examples of general application state. The fifteen-modality target map is an extension roadmap, not current compatibility; token/cost savings still require18.6's complete workflow measurements.

For a viral shared actor, immutable authorized publications can fan reads to bounded serving endpoints;500,000 independent4-KiB responses still transmit2.048 billion bytes. Same-actor writes remain serialized. Historical cold requests park asynchronously; derived analytical views build outside the reactor and expose explicit freshness. A10–30% hot-user assumption alone cannot prove the memory budget.
