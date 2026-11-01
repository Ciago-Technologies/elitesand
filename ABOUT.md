# Ciagomem — SLAE: System Architecture & Internals

> **Implementation status:** This guide includes target architecture and acceptance API examples. Current runnable integration is the [embedded Rust quickstart](ciagomem-engine/examples/quickstart.rs), using non-durable MemoryJournal. Runtime BFBS families, general arrays, network SDKs/server and distributed services remain milestones in specification 14.11. Section 17 is the complete target application/operations contract; target examples are not runnable SDK releases or measured performance guarantees.


This document details the internal engineering architecture of **Ciagomem — SLAE (v3.0; implemented R11 primitives plus planned layers)**. Ciagomem is designed around the **RUM Conjecture** (Read, Update, Memory overhead), utilizing specialized physical representations for resident hot state, durable write-ahead logging, and cold checkpointed extents.

---

## 1. Three-Tier Storage Architecture

```text
                       ┌──────────────────────────────────────┐
                       │     Client SDKs (TS / Go / Rust)     │
                       └──────────┬────────────────┬──────────┘
              Local UDS + memfd   │                │  Remote TLS 1.3 / QUIC
             (Sealed Zero-Copy)   ▼                ▼  (18-Byte Binary Frame)
┌──────────────────────────────────────────────────────────────────────────────┐
│                        Ciagomem Owner-Core Reactors                          │
│  ┌─────────────────────────┐  ┌─────────────────────────┐  ┌──────────────┐  │
│  │   Tier 1: Resident RAM  │  │  Tier 2: Durability     │  │ Tier 3: Cold │  │
│  │  • HugeTLB Slot Arenas  │  │  • Per-Core WAL (v2)    │  │ • A/B Extent │  │
│  │  • 128B SlotHeader      │  │  • CRC32C Envelopes     │  │   Checkpoints│  │
│  │  • Word-Atomic Seqlocks │  │  • io_uring Group Commit│  │ • Roaring &  │  │
│  │  • Delta Tail Overlays  │  │  • Dedup Result Ledger  │  │   SQ8 Indexes│  │
│  └─────────────────────────┘  └─────────────────────────┘  └──────────────┘  │
└──────────────────────────────────────────────────────────────────────────────┘
```

### 1.1 Tier 1: Resident State (HugeTLB Slot Arenas)
Active actors reside in 2 MiB Linux HugePage-backed arenas partitioned across owner CPU cores (`target placement directory -> owner core`).
* **Five Power-of-Two Slab Classes:** Extents are allocated in 5 strict size classes (`class 0..4`): **4 KiB, 8 KiB, 16 KiB, 32 KiB, and 64 KiB** (`4096 << class`).
* **64-Bit Generational `SlotHandle`:** Every resident slot is addressed via a packed 64-bit handle: `[generation: 40 bits | class: 4 bits | index: 20 bits]`. When a slot is evicted and reassigned, its 40-bit generation increments, immediately invalidating stale pointers or ABA references.
* **Fixed-Size Extent Body & Delta Tail Overlay:** Within each extent:
  * Bytes `[0..128)` hold the **Extent Image Header** (CRC32C, 128-bit tenant incarnation, LSN, epoch).
  * Bytes `[128..192)` hold the **64-byte Document Prefix** (`CDOC` magic, family ID, schema version, `base_len`, `tail_len`, `document_version`).
  * Offset `192` stores the verified **Base FlatBuffer** (`base_len` bytes, 8-byte aligned).
  * Immediately following the base image is the **Append-Only Delta Tail**. In-place CiaQL mutations (`SetScalar`, `ArrayAppend`, `ArrayPushBounded`) append compact 8-byte-aligned binary operations to the tail without rewriting the base FlatBuffer. In the target document path, when the tail exceeds **25% of E - 192 bytes**, **64 records**, or **50% superseded garbage**, the owner core automatically compacts the base and tail into a fresh canonical FlatBuffer in bounded scratch memory (promoting to a larger slab class if required).

### 1.2 Tier 2: Durability (Per-Core WAL v2 & Group Commit)
Every owner core manages an independent, append-only Write-Ahead Log (`EnvelopeV2`) over Linux `io_uring`.
* **32-Byte EnvelopeV2 Header:** Protects every log record with hardware-accelerated CRC32C checksums, monotonic Log Sequence Numbers (LSNs), tenant slot bindings, and epoch tags.
* **Compact Binary Mutation Batches:** Instead of logging entire 64 KiB documents on every edit, Ciagomem logs an **88-byte batch header** followed by compact binary opcodes (`OpCode::SetScalar`, `OpCode::ArrayAppend`, etc.) and a **72-byte Idempotency Result Ledger**.
* **Constant-Time Dirty Tracking:** A preallocated, intrusive doubly linked FIFO list (`TruncationPins`, 33 bytes per slot) tracks the oldest uncheckpointed mutation (`rec_lsn`) per tenant in $O(1)$ time with zero heap allocations.

### 1.3 Tier 3: Cold State (A/B Extents & Per-Tenant Indexes)
When the resident working set exceeds physical RAM (e.g., 1,000,000 registered tenants on a 16 GiB node), cold actors are evicted to NVMe storage using an **A/B Ping-Pong Extent Protocol**.
* **Crash-Safe Checkpoints:** Checkpoints always write to the *inactive* extent copy (`copy 0` or `copy 1`). The route flips to the new copy only after a full-length successful `io_uring` CQE; image CRC32C is validated on load/recovery.
* **Per-Tenant Indexing:** The target shard maintains bounded, tenant-isolated indexes; current reference indexes use ordinary owned Rust structures:
  * **Roaring Bitmaps:** Compressed equality candidate sets for rapid filter pushdown.
  * **64-Slot Numeric Zone Maps:** Min/Max zone bounds evaluated in 4-lane SIMD batches (`wide::i64x4`).
  * **BM25 Inverted Postings:** Term-frequency and document-length postings for full-text search.
  * **SQ8 SIMD Vector Shortlists:** Symmetric signed 8-bit quantized vectors (`wide::i32x8` dot-product accumulation) paired with full-precision reranking.

---

## 2. Concurrency, Memory Layout & R11 Safety Invariants

### 2.1 The 128-Byte `SlotHeader` & 64-Byte `CoreSlotMeta` Sidecar
Ciagomem enforces strict cache-line discipline:
* **`SlotHeader` (`128 bytes`, `align(64)`):** Contains ten 8-byte atomic fields (`seq`, `owner`, `tenant`, `applied_lsn`, `checkpoint_lsn`, `payload_len`, `last_generation`, `incarnation_lo`, `incarnation_hi`, `reserved`) plus 48 bytes of alignment padding.
* **Target `CoreSlotMeta` (`64 bytes`, `align(64)`; not implemented):** To prevent polluting the hot `SlotHeader` cache lines, scaling and eviction telemetry (`placement_epoch`, `migration_id`, `admitted_ns`, `last_access_ns`, TinyLFU `frequency`, and segmented CLOCK flags) are isolated in a core-local sidecar array.

### 2.2 Boehm-Correct Seqlocks & `WriterStalled` Deadlines
Payloads are stored as `AtomicU64` words. Writers increment `seq` to an odd number (with `Acquire` CAS and `Release` fence), write precomputed 8-byte words, and commit an even `seq` (`Release`).
* Readers validate snapshots Lock-Free using `Acquire` fences and a **2 µs read deadline**.
* If a writer is delayed by OS preemption for >1 µs, readers observe `DeadlineReadError::WriterStalled` and safely retry or fall back without declaring false data corruption.
* Under high read contention (>5% fallback ratio over 30,000 reads), the target host may promote hot actors to **Left-Right immutable snapshot publication** or coalesce via the owner-local `ReadCombiner`.

### 2.3 CPU/DMA Exclusion & Single-Use `PinToken` (R11 Hardening)
To prevent torn disk images and use-after-free bugs during asynchronous `io_uring` operations:
1. **Exclusive Access Word Sidecar:** Each slot has an engine-private `AtomicU64` access word (`bit 63 = CLAIMED`, `bit 62 = DEVICE`, `bits 0..61 = active CPU reader count`).
2. **DMA Exclusion:** Before queuing a checkpoint write or cold load to `io_uring`, the lifecycle atomically transitions the slot from `CLAIMED` (zero active CPU readers) to `CLAIMED | DEVICE`. While `DEVICE` is set, safe public reads return `WriterActive` for the matching owner or `Reassigned` for a revoked/unowned handle; public mutations of claimed slots return `WriterBusy`. The unsafe backend still must uphold its completion/pointer contract. This is not a proof covering arbitrary unsafe host code.
3. **Checked Single-Use `PinToken`:** Eviction protection uses a move-only, non-cloneable `PinToken` bound to the full 64-bit `SlotHandle` and a monotonic lifecycle owner ID. The 16-bit pin counter uses checked addition (saturating cleanly at `65,535` pins to prevent overflow wrap-around evictions), and `unpin(token)` consumes the token at compile time so duplicate or stale releases cannot strip protection from a recycled slot.

### 2.4 Target Anti-Thrashing: TinyLFU + Segmented CLOCK
To protect a 16 GiB node from cache-thrashing during viral traffic spikes or large scans:
* **Count-Min Sketch (TinyLFU):** Each core maintains a 640 KiB frequency sketch (4 rows × $2^{18}$ 4-bit counters + $2^{20}$-bit doorkeeper). Cold candidates are admitted into the resident arena only if their estimated frequency beats the selected eviction victim.
* **Segmented CLOCK & Dwell Hold-Down:** Resident slots are divided into a **20% Probation** segment and an **80% Protected** segment, with a mandatory **100 ms minimum dwell time** (`min_dwell_time_ms`, target policy) before a newly admitted slot can be evicted by capacity pressure.

---

## 3. Security, Capabilities & CaMeL AI Governance

### 3.1 Conjunctive HMAC-SHA256 Macaroons
Every connection and request is authenticated using cryptographic capabilities (`Macaroon`) verified via constant-time `HMAC-SHA256`. A capability is valid only if all mandatory caveats hold simultaneously:
* `Tenant(u128)` (Full 128-bit random incarnation)
* `Namespace(u32)` & `AllowedFamilies`
* `Session(u128)` & `ClientId(u64)`
* `BootEpoch(u128)` (OS-random epoch generated per server startup to prevent cross-restart replay)
* `PeerUid(u32)`, `Audience(u64)`, `Ops(u64)`, `KeyEpoch(u64)`, and `ExpiresAt(u64)`.

### 3.2 CaMeL Information Flow Lattice
When Ciagomem is used as an AI Agent memory store, every record carries a private `Label` governing its **Integrity** and **Confidentiality (Reader Set)**:
* **Integrity Ordering:** `External < AgentDerived < UserDirect < Policy`
* **Provenance Attestation:** Untrusted network or LLM outputs default to `External`. Elevating data to `AgentDerived`, `UserDirect`, or `Policy` requires verified **Ed25519 content-bound signatures** from a trusted issuer and single-use, session-bound approvals.
* **Lattice Join Rule:** When an AI query combines multiple actors, the output automatically inherits the **minimum integrity**, the **union of provenance categories**, and the **intersection of allowed 128-bit reader principals**—preventing untrusted external web data from hijacking privileged agent instructions or leaking across tenants.

### 3.3 Local Zero-Copy `SealedExportWindow` (`memfd`)
For local agent processes running on the same Linux host, Ciagomem can bypass socket payload copying by passing a read-only file descriptor over Unix Domain Sockets (`SOCK_SEQPACKET` + `SCM_RIGHTS`). The memory window is backed by a kernel-sealed `memfd` (`F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_FUTURE_WRITE | F_SEAL_SEAL`) and verified against `mprotect` write-upgrade attacks before exposure.
R11 access-word sidecars are implemented and cost 8 bytes per allocated slot outside the registered backing. The 64-byte CoreSlotMeta, TinyLFU/segmented CLOCK, document overlay, networking and Left-Right publication described above remain planned. See specification 16 for abandonment retention and pin-token ownership.
