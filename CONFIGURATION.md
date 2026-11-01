# Configuration and memory planning

**Current:** the Rust crates use constructor/config structures, including Engine Limits, LifecycleConfig and WAL LogConfig. There is no server consuming the environment variables below. **Target:** the host contract in specification 17.5, with validation before listener readiness or LSN assignment.

## Current local configuration

Start from the complete [Rust quickstart](ciagomem-engine/examples/quickstart.rs). It uses Limits::default() and a bounded 8-MiB MemoryJournal. Inspect each crate's public structs for its current fields. Setting CIAGOMEM_MODE or CIAGOMEM_ARENA_GIB does not turn that example into a server or make its journal durable.

## Target host settings

Precedence is configuration TOML -> recognized environment overrides -> explicit CLI flags. Reject unknown project keys, duplicate file keys, invalid enums, overflow, conflicting aliases and unsafe paths. `--check-config` prints redacted effective values without modifying storage.

| Variable | Target default | Type and validation |
| --- | --- | --- |
| CIAGOMEM_MODE | development | development or production; production is explicit, not an insecure fallback |
| CIAGOMEM_PROFILE | custom in development; storage-service in production | custom, embedded-agent, storage-service; profile fixes budget regions before overrides |
| CIAGOMEM_NODE_ID | 1 | u16 in1..65534, directory-unique; no silent ID reuse |
| CIAGOMEM_REACTOR_CORES | min(14, available cpuset CPUs) | integer1..64 and <=allowed CPUs; affinity setup must succeed in production |
| CIAGOMEM_ARENA_GIB | 2 development;12 storage-service;10.1875 embedded-agent | nonnegative decimal GiB, parsed exactly to whole MiB using rational arithmetic, no binary floating rounding; round down only where explicitly reported |
| CIAGOMEM_BIND_ADDR | 127.0.0.1:7443 development; explicit in production | numeric IP/port; production requires TLS regardless of bind address; separate QUIC enable flag defaults false |
| CIAGOMEM_QUIC_ENABLED | false | bool true/false; enable only with qualified TLS/QUIC host and explicit UDP listener |
| CIAGOMEM_UDS_PATH | /run/ciagomem/db.sock | absolute path with trusted parent; SO_PEERCRED required; never unlink an unrelated existing file |
| CIAGOMEM_DATA_DIR | /var/lib/ciagomem | canonical absolute private path; explicit writable ownership, exclusive node lock and qualified WAL/extent devices |
| CIAGOMEM_BACKUP_DIR | /var/backups/ciagomem | canonical allowed root; export uses the hardened directory policy; never follows an attacker-controlled replacement |
| CIAGOMEM_DEFAULT_DURABILITY | local-durable | applied, local-durable, replicated-durable; unsupported tier rejects rather than downgrading |
| CIAGOMEM_TLS_CERT / CIAGOMEM_TLS_KEY / CIAGOMEM_CA_CERT | absent | mandatory for production remote listener; nonempty readable validated PEM, matching cert/key, trusted CA, principal mapping |
| CIAGOMEM_KEYRING_FILE | absent | mandatory production issuer/key policy; owner-only secret file, no inline secret environment value; provider-specific anchor remains a separate deployment gate |
| CIAGOMEM_EXPORT_BUDGET_MIB | 64 production;4 development | integer>=0; zero disables FD exports; <=charged I/O region minus reserved completion/staging resources |
| CIAGOMEM_MIN_DWELL_MS | 100 | bounded integer1..60000 for capacity eviction; deletion/repair/shutdown uses separately authorized lifecycle rules |
| CIAGOMEM_LOG_LEVEL | info | error, warn, info, debug, trace; redaction applies at every level |

Production requires qualified recovery/identity anchors, journal/catalog/dedup, clock and key policy, TLS principal mapping, storage and resource admission. Failure must not select weaker durability, erase identities or announce readiness. Configuration is immutable except logging and explicit atomic key-policy reload; other changes require a qualified restart/migration.

## Complete16-GiB planning profiles

| Region, MiB | Embedded-agent | Storage-service |
| --- | ---: | ---: |
| Arena including separate header rounding | 10432 | 12288 |
| Kernel/network/page tables | 2048 | 1536 |
| App/agent processes | 2048 | 1024 |
| Control/metadata/indexes, with embedded buffers included | 1036 | 768 |
| Separate WAL/I/O/export/staging region | 0 | 256 |
| Contingency | 820 | 512 |
| Total | 16384 | 16384 |

These are planning profiles, not qualified deployments. Charge R11 access sidecars at 8 bytes per allocated slot outside region_bytes, plus planned CoreSlotMeta64, dirty tracking33, routes, labels, indexes and allocator overhead. Do not allocate maximal indexes/clients/windows without quota accounting. Contingency is not free steady-state capacity.

## Target slab capacities

ExtentHeader128 and CDOC64 are inside E; SlotHeader128 is separately allocated and HugePage-rounded. For a homogeneous12-GiB arena across14 cores:

| Class | E bytes | Maximum root/tail region before padding | Rounded arena-only resident ceiling |
| --- | ---: | ---: | ---: |
| 0 | 4096 | 3904 | 3045376 |
| 1 | 8192 | 8000 | 1547776 |
| 2 | 16384 | 16192 | 779264 |
| 3 | 32768 | 32576 | 391424 |
| 4 | 65536 | 65344 | 196160 |

Ceilings do not include metadata/connection affordability outside the arena; mixed classes have a different capacity equation. Document growth triggers target compaction/promotion; an image that cannot fit the largest extent rejects. This path is not yet integrated in the reference Engine.

## Target durability and session budgets

Applied is explicitly non-durable. Local-durable requires the qualified local WAL and result ledger persistence boundary. Replicated-durable additionally requires current-term consensus quorum persistence; unsupported mode rejects. Default production mode is local-durable only once its adapter is delivered.

64 MiB admits at most16384 raw4-KiB export windows, not16384 sessions. Allocate windows lazily; extra sessions use framed bytes or receive Busy. Count sockets, TLS state, mappings and descriptors separately. Registered tenants, resident actors, active sessions and requests/sec are independent measurements. No latency or million-user guarantee follows from this table.

## Current MemoryBroker limits

The Rust memory module accepts MemoryLimits, not server environment variables. Defaults:64 tasks,16 sources/task,1-MiB source images, one source read,4096-byte summary,8192-byte context,8 context refs,300-second worker lease and900-second summary TTL. Engine limits apply as well. Explicit forget releases task metadata and journal-deletes outputs; there is no background retention service. Caller-held snapshots, broker instances and actual model token/cost budgets require host limits. See specification18.2 for constructor ceilings.

## Target gateway and query-worker resource reservations

Specification15.13–15.18 adds deployment requirements, not new implemented environment variables. Configure measured limits separately for gateway connections/sessions/pending bytes, pooled backend streams, per-core producer lanes, aggregate ingress descriptors, query jobs/CPU, snapshot/pin count and duration, spill/cache/output bytes and per-tenant work. A per-core4,096-entry data budget is shared across producers; it must not grow with gateway count. Reserve completion/control capacity independently.

The existing profile is16 GiB, not decimal16 GB. Colocated query workers must fit its regions and CPU allocation by reducing other reservations; do not add worker pools above a12-GiB arena and an already full node budget. Remote gateways/query nodes have separate RAM/CPU budgets and increase total deployment cost. Multiplexing relocates socket state rather than removing it. Full queues return Busy before acceptance; live accepted mutations retain reconciliation state.

One million pending requests at a hypothetical1 KiB of state each already require about0.954 GiB before payloads and transport overhead. One million4-KiB export pages require3.815 GiB before descriptor/mapping costs; the existing64-MiB export cap remains16,384 windows. Exceeding any budget reduces admission, never silently borrows the contingency region. A configuration must declare accepted outstanding capacity, sustainable throughput/SLO and failure reserve per node; user count alone does not size a cluster.

For the density objective,10–30% hot is a measured actor/byte working set, not a guaranteed capacity percentage. At300,000 resident64-KiB actors, extents alone require18.311 GiB. Account for cold identity catalogs as well as hot state. A stateless gateway still owns transient connections; database pooled sockets, transport, authentication and I/O buffers remain charged inside16 GiB. Reserve cold-fault continuation/staging/completion capacity before admission.
