# Production Use Cases & Architecture Blueprints

> **Implementation status:** This guide includes target architecture and acceptance API examples. Current runnable integration is the [embedded Rust quickstart](ciagomem-engine/examples/quickstart.rs), using non-durable MemoryJournal. Runtime BFBS families, general arrays, network SDKs/server and distributed services remain milestones in specification 14.11. Section 17 is the complete target application/operations contract; target examples are not runnable SDK releases or measured performance guarantees.


Ciagomem — SLAE is designed to replace fragmented multi-database architectures (such as running PostgreSQL + Redis + Pinecone + Kafka simultaneously) with a single, unified actor data plane.

Below are four target workload blueprints showing how to model schemas, configure durability, and execute CiaQL pipelines for real-world workloads.

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
## General application CRUD and telemetry

Users, sessions, orders, projects, sensor readings and other custom families follow the same schema/CRUD model; gaming and wallets are examples, not a specialization boundary. Applications choose keys, fields, relationships, expiry/retention rules and business invariants. Large blobs and unbounded event histories need an explicit external/chunking design beyond a single 64 KiB extent. The target feature/qualification checklist is specification 17.8.
