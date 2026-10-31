# CiagoMem — SLAE (Unified State-Log Actor Engine)

**Version:** `0.3.0` (Architecture v3.0)  
**Product:** CiagoMem — SLAE ("The Store")  
**Query Language:** CiaQL (Ciagomem Query and Cognitive Language)  
**Provider:** Ciago Technologies  

---

## What is CiagoMem?

**CiagoMem — SLAE (State-Log Actor Engine)** is an ultra-low-latency, actor-native storage engine and cognitive data plane designed for high-concurrency cloud applications, real-time telemetry, and stateful AI agents. It serves as the core persistence foundation for the **Cloud Native Computing Platform (CNCP)** and the **CoveStack** infrastructure ecosystem.

Modern applications suffer from the **"Multi-Database Tax"**: developers are forced to maintain a relational database for structured state, an in-memory cache for active sessions, and a separate vector database for AI embeddings. Every request pays the penalty of network hops, JSON/Protobuf serialization, and eventual-consistency bugs across disparate systems.

**CiagoMem collapses this stack into a single memory boundary.** By partitioning data into isolated, single-writer **Actors** organized by **Families** (collections) and backed by a thread-per-core Linux `io_uring` architecture, CiagoMem executes relational CRUD, array/ring-buffer mutations, cross-actor joins, and SIMD-accelerated vector similarity searches inside the same engine at sub-microsecond speeds.

---

## Core Concepts

* **Actors & Families:** Data is partitioned into **Actors** (individual records, user accounts, or autonomous AI agents) grouped by **Families** (collections/tables such as `users`, `sessions`, `orders`, or `agent_memories`) inside an authenticated tenant namespace.
* **CiaQL (Ciagomem Query and Cognitive Language):** A unified, pipeline-driven query language (`|>`) for executing atomic in-place mutations, relational joins, grouped aggregations, and hybrid BM25 + vector similarity searches directly against actor memory.
* **FlatBuffers First:** CiagoMem eliminates ORM and serialization overhead. Application schemas are defined using FlatBuffers (`.fbs`), compiled into binary reflection descriptors (`.bfbs`), and registered dynamically with the engine—enabling zero-copy reads, strict type safety, and lazy schema evolution.
* **CaMeL Provenance & AI Governance:** Every actor and memory frame is protected by a cryptographic information-flow lattice (`External < AgentDerived < UserDirect < Policy`), preventing prompt-injection data laundering and unauthorized tool side effects.
* **Hardware-Sympathetic Concurrency:** Built on a thread-per-core reactor model using Boehm-correct word-atomic Seqlocks, Lamport SPSC meshes, and CRC32C group-commit Write-Ahead Logging (WAL v2) over `io_uring`.

---

## Why CiagoMem? (Architectural Advantages)

| Capability | Traditional Multi-DB Stack | CiagoMem — SLAE |
| :--- | :--- | :--- |
| **Data Model** | Fragmented across SQL tables, KV caches, and Vector indexes | Unified **Actor Document + Vector + Delta Tail** in one extent |
| **Read Latency (Hot)** | 500 µs – 5 ms (Network + Serialization) | **p99 ≤ 200 ns** (In-engine seqlock) / **≤ 300 ns** (Local `memfd` export) |
| **Write Path** | B-Tree page splits / Heavy ORM transactions | Append-only **Delta Tail Overlay** + Group-Commit `io_uring` WAL |
| **Serialization Tax** | JSON / Protobuf encode & decode on every hop | **Zero-Copy FlatBuffers** verified at ingestion |
| **AI Memory Safety** | Unverified text blobs vulnerable to prompt injection | **CaMeL Lattice** with Ed25519 content-bound provenance |
| **Concurrency Control** | Heavy mutex locks and MVCC vacuuming overhead | **Lock-free Seqlock readers** + single-writer owner cores |

---

## Primary Use Cases

1. **Stateful AI Agents & Cognitive Memory:** Give LLMs and autonomous agents a dedicated actor context where conversation transcripts (BM25 text search), semantic embeddings (SQ8 SIMD vector search), session quotas (scalar counters), and security guardrails (CaMeL provenance labels) live side-by-side without network hops between a database and a vector store.
2. **High-Throughput Gaming, FinTech & Session Stores:** Utilize lock-free actor slots, optimistic document versioning, and bounded ring-buffer arrays (`push_bounded`) to track real-time wallets, leaderboards, telemetry streams, and active user sessions at sub-microsecond latencies.
3. **Enterprise Edge-to-Cloud Hybrid Workloads:** Deploy CiagoMem as a lightweight container via **CoveStack** on developer laptops and edge nodes, or scale out across **CNCP** cloud micro-VMs with epoch-fenced routing and deterministic Calvin transactions.

---

## Quickstart Overview

1. **Run the Server:** Start the pre-built CiagoMem container via Docker or Docker Compose (see [INSTALL.md](INSTALL.md)).
2. **Define Your Schema:** Create an `app_schema.fbs` file and compile it to `.bfbs` and your target language bindings using `flatc` (see [EXAMPLES.md](EXAMPLES.md)).
3. **Connect & Execute:** Use the official **TypeScript (`@ciagomem/sdk`)**, **Go (`ciagomem-go`)**, or **Rust (`ciagomem-client`)** SDK to register your schema, insert FlatBuffer documents, and run CiaQL pipelines.

```text
scan family("users")
  |> filter (state.credits > 50 and state.status == "ACTIVE")
  |> group_by (state.tier)
  |> aggregate {
       total_users: count(),
       avg_credits: avg(state.credits)
     }
  |> take 10
```

---

## Documentation Directory

* **[ABOUT.md](ABOUT.md):** Deep-dive into storage architecture, memory layout, seqlock concurrency, DMA pin safety, and CaMeL security.
* **[INSTALL.md](INSTALL.md):** Production Docker & Docker Compose deployment, Linux host tuning (`io_uring`, `hugetlbfs`), and SDK installation.
* **[EXAMPLES.md](EXAMPLES.md):** Step-by-step schema compilation, end-to-end TypeScript/Go/Rust CRUD examples, CiaQL mutation reference, hybrid vector search, and cascade deletions.

---

## License & Proprietary Notice

Copyright © 2026 **Ciago Technologies**. All rights reserved.  
CiagoMem — SLAE binaries, container images, and official SDKs are distributed under the Ciago Technologies Enterprise & Commercial License.