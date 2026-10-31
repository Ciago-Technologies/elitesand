# Configuration, Environment Variables & Capacity Planning

This guide documents all server environment variables, memory budget allocations, slab class sizing, and durability tiers for **CiagoMem — SLAE**.

---

## 1. Server Environment Variables

Configure the CiagoMem container using the following environment variables in your `docker run` command or `docker-compose.yml`:

| Variable | Default | Description |
| :--- | :--- | :--- |
| `CIAGOMEM_NODE_ID` | `1` | Unique 16-bit node identifier (`1..65534`; `0` and `65535` are reserved). |
| `CIAGOMEM_MODE` | `production` | `production` (enforces TLS 1.3 and HugeTLB checks) or `development` (allows heap fallback). |
| `CIAGOMEM_REACTOR_CORES` | `14` | Number of pinned thread-per-core `io_uring` reactors (`1..64`). |
| `CIAGOMEM_ARENA_GIB` | `12` | Total RAM (in GiB) reserved for resident actor slot arenas (`10` or `12` GiB on a 16 GiB host). |
| `CIAGOMEM_BIND_ADDR` | `0.0.0.0:7443` | TCP/QUIC listen address for the binary wire protocol. |
| `CIAGOMEM_UDS_PATH` | `/run/ciagomem/db.sock` | Local Linux `SOCK_SEQPACKET` path for co-located zero-copy clients. |
| `CIAGOMEM_DATA_DIR` | `/var/lib/ciagomem` | Root directory for per-core WAL v2 logs, A/B extents, and catalog state. |
| `CIAGOMEM_BACKUP_DIR` | `/var/backups/ciagomem` | Canonical local root allowed for operator `EXPORT` backup rules. |
| `CIAGOMEM_DEFAULT_DURABILITY` | `local-durable` | Default commit tier: `applied` (`0`), `local-durable` (`1`), or `replicated-durable` (`2`). |
| `CIAGOMEM_TLS_CERT` | *Required in prod* | Path to server TLS 1.3 certificate PEM file. |
| `CIAGOMEM_TLS_KEY` | *Required in prod* | Path to server TLS 1.3 private key PEM file. |
| `CIAGOMEM_CA_CERT` | *Required in prod* | Path to trusted CA bundle PEM for client mTLS verification. |
| `CIAGOMEM_EXPORT_BUDGET_MIB` | `64` | Maximum RAM reserved for active 4 KiB `memfd` export windows (`64 MiB` = max 16,384 windows). |
| `CIAGOMEM_MIN_DWELL_MS` | `100` | Minimum hold-down time (ms) before a newly admitted slot can be evicted by capacity pressure. |
| `CIAGOMEM_LOG_LEVEL` | `info` | Logging verbosity: `error`, `warn`, `info`, `debug`, `trace`. |

---

## 2. 16 GiB Node Memory Budget Profiles

CiagoMem provides two validated memory planning profiles for a standard **16 GiB (`16,384 MiB`)** node:

| Subsystem Region | Embedded-Agent Profile (`MiB`) | Storage-Service Profile (`MiB`) | Purpose |
| :--- | ---: | ---: | :--- |
| **Engine Resident Arena** | `10,432` | `12,288` | 2 MiB HugePages holding `SlotHeader` (128B) + Actor Extents (4–64 KiB). |
| **Kernel, Sockets & Page Tables** | `2,048` | `1,536` | OS slab, TCP/TLS socket buffers, `io_uring` pinned rings. |
| **Agent / App Processes** | `2,048` | `1,024` | Co-located application or AI worker cgroup allocation. |
| **Control, Sidecars & Indexes** | `1,036` | `768` | `CoreSlotMeta` (64B/slot), TinyLFU sketches (640 KiB/core), Roaring/SQ8 indexes, dirty lists (33B/slot). |
| **WAL, I/O & Export Buffers** | *(included above)* | `256` | Group-commit DMA scratch buffers + 64 MiB `memfd` export budget. |
| **Contingency Reserve** | `820` | `512` | Unallocated safety margin (>3–5%). Never assigned as steady-state capacity. |
| **Total Node RAM** | **16,384 MiB** | **16,384 MiB** | Strict hardware budget. |

---

## 3. Slab Classes & Resident Slot Capacity

Within the **12 GiB (`12,288 MiB`)** Storage-Service Arena across **14 reactor cores**, every actor is allocated into the smallest fitting power-of-two slab class (`class 0..4`). Each slot includes a 128-byte `SlotHeader`, a 128-byte `ExtentHeader`, and a 64-byte `CDOC` prefix (leaving `E - 192` bytes for the FlatBuffer payload and delta tail):

| Class | Extent Size (`E`) | Max Usable Payload + Tail | Max Resident Actors (Homogeneous 14-Core Node) |
| ---: | ---: | ---: | ---: |
| `0` | **4 KiB** (`4,096 B`) | `3,904 Bytes` | **3,045,376 slots** |
| `1` | **8 KiB** (`8,192 B`) | `8,000 Bytes` | **1,547,776 slots** |
| `2` | **16 KiB** (`16,384 B`) | `16,192 Bytes` | **779,264 slots** |
| `3` | **32 KiB** (`32,768 B`) | `32,576 Bytes` | **391,424 slots** |
| `4` | **64 KiB** (`65,536 B`) | `65,344 Bytes` | **196,160 slots** |

*Note: If a mutation causes an actor's compacted FlatBuffer to grow beyond its current class (e.g., from 4 KiB to 8 KiB), the owner core automatically promotes the actor to the next slab class and issues a new generational `SlotHandle`. Documents exceeding 64 KiB return `Status 7 (DocumentTooLarge)`.*

---

## 4. Durability Tiers (`requested_durability`)

Every mutation request can specify its required durability guarantee:

1. **`0` — `Applied` (In-Memory Seqlock Only):**
   * Returns as soon as the owner core updates the resident seqlock slot.
   * **Latency:** Sub-microsecond.
   * **Use Case:** Ephemeral telemetry or non-critical cache state. Not restart-safe.
2. **`1` — `LocalDurable` (Default Production Mode):**
   * Returns only after the compact WAL v2 batch and idempotency result ledger are persisted to the local PLP NVMe drive via `io_uring` group commit.
   * **Latency:** Target `p99 ≤ 50 µs`.
   * **Use Case:** Standard single-node ACID mutations and durable retries.
3. **`2` — `ReplicatedDurable` (Cluster Quorum Mode — `RPO = 0`):**
   * Returns only after local NVMe persistence **plus** synchronous quorum acknowledgement (`Q = 2 of 3` replicas) in the current placement term.
   * **Use Case:** Mission-critical financial ledgers and zero-data-loss failover.