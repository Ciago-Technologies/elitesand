# Ciagomem — SLAE: Technical Whitepaper and Master Engineering Specification

**Version:** 3.0, R9 remediation revision — 31 October 2026\
**Product:** Ciagomem — SLAE, the Unified State-Log Actor Engine (“The Store”)\
**Language:** CiaQL — Ciagomem Query and Cognitive Language\
**Status:** Architecture and component implementation; **not signed off for production**\
**Workspace:** [`ciagomem-slae`](README.md), package version `0.3.0`

This revision incorporates the [R9 verification report](review/r9/Ciagomem-SLAE-R8-Remediation-Verification.md.txt) and current R9 patch ZIP. Earlier reports are retained in [review/reports](review/reports), with patch/probe/log evidence under review and original extracted source evidence under review/initial-source. Superseded ZIPs and extracted workspaces were removed after preservation; see the [cleanup manifest](docs/cleanup-manifest.json). The current workspace is authoritative; historical evidence is not source to copy over it.

“Must” describes a design requirement. “Implemented” identifies code present in the workspace. Targets and proposed deployment settings are not measurements or production guarantees. No Miri, cross-architecture, power-cut, or performance result from the supplied review is represented as a result obtained for this revision.

## Contents

1. [Scope and review disposition](#1-scope-and-review-disposition)
2. [Storage architecture](#2-storage-architecture)
3. [Memory and capacity](#3-memory-and-capacity)
4. [Concurrency and progress](#4-concurrency-and-progress)
5. [Identity, slots, and routing](#5-identity-slots-and-routing)
6. [I/O, checkpoints, and the write-ahead log](#6-io-checkpoints-and-the-write-ahead-log)
7. [Transactions and recovery](#7-transactions-and-recovery)
8. [Authorization, capabilities, and exports](#8-authorization-capabilities-and-exports)
9. [Information flow and AI frames](#9-information-flow-and-ai-frames)
10. [CiaQL grammar and policy](#10-ciaql-grammar-and-policy)
11. [Operations, backup, and evolution](#11-operations-backup-and-evolution)
12. [Workspace and reference implementation](#12-workspace-and-reference-implementation)
13. [Verification and production exit criteria](#13-verification-and-production-exit-criteria)

## 1. Scope and review disposition

The architecture combines actor-local state, append-only durability, deterministic multi-actor coordination, and labelled AI memory. Its operating objective is one million **registered** tenants on a 16 GiB node, with a bounded resident working set. Registered tenants, resident actors, and concurrent agent sessions are separate capacity dimensions.

The previous document incorrectly described a small monolithic reference kernel as a complete production engine. In particular, its embedded crate did not contain the WAL, eviction state machine, reactor, replay system, or actual SPSC rings described in the prose. This workspace retains the P0 WAL, lifecycle, ring, and simulation modules alongside a reviewed v3 API, but these components do not constitute a fully integrated service.

### R9 revision — 31 October 2026

The [R9 ledger](docs/R9_REMEDIATION.md) supersedes the earlier advisory publisher and teardown contracts. Durable publication outside core now requires its one DurabilityPublisher; raw bind/unbind/publish methods are crate-private. Logs own the publisher through initialization and runtime. Pending-I/O owner destruction fences the core and retains the referenced buffers instead of freeing them. Arena pending counters cover lifecycle submissions/completions. Lifetime tracking is provisionally armed before backend submission, rolled back on rejection, and retained on unwind. CQE handlers require an unsafe proof of genuine backend completion; raw numeric tokens are not safe evidence that buffers can be released. A new submission invalidates an older device-release acknowledgement. This is a memory-safety fallback with an explicit retention cost, not the missing production cancel/reap driver. The obsolete R8 ZIPs were removed after preserving evidence; one policy file and one review prompt remain.

### R8 revision — 31 October 2026

The [R8 ledger](docs/R8_REMEDIATION.md) supersedes affected earlier contracts. Logs hold the core's only DurabilityPublisher; direct publish/bind/unbind are crate-private. Duplicate binding and shorter-than-published recovery reject. Publisher Drop releases the claim on normal teardown and failed initialization. A pending-I/O log or lifecycle drop fences the core and retains its referenced buffers. Exports check boot and expiry using trusted current time. Engine retains Send + Sync when its journal does, with a thread-safe clock and atomic time high-water mark. No dependency or persistent-format change is introduced. There is one engineering policy.json and one current review prompt; obsolete archives/extracted copies were cleaned while preserving review/source evidence.

### R7 revision — 31 October 2026

The [R7 ledger](docs/R7_REMEDIATION.md) supersedes affected R6 contracts. The supplied three-fix patch was applied after reproducing its probes. Additional changes enforce engine entry-time capability expiry/boot checks and bind the WAL exclusively to its core controls, rejecting a second log or a published watermark beyond recovery, and publish recovered durability after superblock confirmation. A startup validator detects unrelated arena control arrays; the absent node host must still invoke it for every class. No persistent format change or new dependency is introduced.

### R6 revision — 31 October 2026

The [R6 disposition ledger](docs/R6_REMEDIATION.md) supersedes affected R5 contracts. All eight supplied probes reproduced their described behavior before this correction. R6-N1 through N7 now have corrected executable contracts: shared cross-class core controls, no derived-label attestation laundering, random incarnations, boot-bound and tenant-budgeted replay protection, epoch-aware image classification, WAL-coverage-gated recovery and explicit owner checkpoint barriers.

This revision changes arena constructors, capability/provenance/approval APIs and framing, replay tracker inputs and load admission. Update callers; old signed objects cannot bypass new mandatory boot binding. Production anchor, WAL replay driver, reactor scheduling, repair and storage qualification remain open. The R6 reviewer's Miri and ARM compile results apply to the preceding source, not these changes.

### R5 revision — 31 October 2026

The supplied R5 deep-research report and harness review an older four-crate bundle. This revision retains the newer five-crate actor-native execution milestone and applies the relevant R5 corrections to it. The [R5 disposition ledger](docs/R5_REMEDIATION.md) lists every finding, executable guards and remaining gates. Original reproduction assertions are preserved as text, because many assert that a vulnerability exists and therefore cannot serve as positive fix tests.

This revision changes identity metadata, reference literals, label/endorsement APIs, canonical slot types and checkpoint-image format. It is a breaking component revision; no automatic migration or production compatibility guarantee is implied. R5 findings are not all production-closed: the anchor provider, integrated durable deletion/recovery, fleet/S3 adapters, reactor control scheduling and hardware qualification remain explicit gates.

### 1.1 Review resolution matrix

| Review finding                                        | Corrective implementation                                                                                                                       | Remaining boundary                                                                                              |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| Missing lifetime and unstable `str::as_str`           | Replace the substring parser with the existing typed P0 lexer/parser and extend its grammar                                                     | Unsupported syntax returns an error                                                                             |
| S1 / P0-A: foreign reads and fleet scans              | Policy requires a verified session, a trusted actor-owner resolver, operation scopes, and operator scope for scans                              | Every executor entry point must invoke policy per request                                                       |
| S2 / P0-B: foreign actors hidden in raw bodies        | Typed field paths and mutation ASTs; participant and owner checks include qualified references                                                  | An actor-native reference executor is supplied; arbitrary expression evaluation remains unsupported             |
| S3: named argument smuggling, defaults and limits     | Token-based named arguments; duplicate and malformed arguments rejected; `top_k` bounded to 1–1,000                                             | The reference executor enforces work/result/group/join limits; node allocator enforcement remains open          |
| S3b: discarded vector/metric                          | Similarity AST retains the numeric vector, metric, and threshold                                                                                | Runtime `$q` vector bindings are rejected until implemented                                                     |
| S3c/S3d: stage splitting and TAKE/EXPORT disagreement | Parser consumes grammar productions; TAKE may be followed only by one final EXPORT                                                              | Typed JOIN/LINK and explicit schema upgrade APIs are available; unsupported administration commands fail closed |
| S4 / P0-F: unrestricted S3 prefixes                   | Exact bucket and path-boundary matching, strict canonical destination grammar                                                                   | Operator-managed configuration and storage transport remain external                                            |
| S5 / P0-C: incarnations repeat on restart             | Persistent create/lookup/delete journal with hash chain and independent rollback anchor; only unacknowledged tail bytes may be discarded        | Deploy a durable independent anchor and integrate identity metadata with WAL/backup                             |
| S6 / P0-D: forgeable endorsements                     | Ed25519 provenance and content/field/action/session-bound single-use approvals; private labels                                                  | Trusted issuer, session and nonce lifecycles must be integrated                                                 |
| S6b / P0-E: unsigned or unrestricted caveats          | HMAC chain; mandatory tenant, expiry, UID, session, audience, boot epoch, operation and key-epoch caveats; private verified-session fields      | Authentication transport and key rotation are not a server implementation                                       |
| S7: 64-principal aliasing                             | Sorted explicit 128-bit principal IDs, maximum four readers, fail closed beyond that bound                                                      | Larger sets require an authenticated external representation                                                    |
| S8: home-grown ARX                                    | RustCrypto HMAC-SHA256 in a dedicated crypto crate, with known-answer testing                                                                   | SHA-256 content binding is supplied; no vault service is supplied                                               |
| S9/S10: unused deadline and false poison              | Both arena read paths call deadline readers; a delayed writer is `WriterStalled`                                                                | Portable `Instant` clock remains; calibrated counters and attach-time repair remain open                        |
| S11: unbounded overflow and credits                   | Overflow rejects with ownership of the unsent item; cumulative consumption acknowledgements are idempotent; separate control/data mesh capacity | Reactor control-priority dispatch remains open                                                                  |
| S12: linear dirty scans                               | Preallocated intrusive first-dirty FIFO; constant-time insert, unlink and head decision                                                         | Monotonic per-core LSN contract enforced; no measured 50 ns claim                                               |
| Vote-slot ABA                                         | Single-owner mutable vote table; tagged ballots and duplicate rejection precede state mutation                                                  | Sequencer and message delivery are separate work                                                                |
| Unbudgeted export windows                             | Shared byte/session budget reserved before allocation, released on error/drop                                                                   | Session manager must share one authoritative budget and handle FD/VMA limits                                    |

See the [remediation ledger](docs/REVIEW_REMEDIATION.md) for implementation paths, tests, and open production gates.

## 2. Storage architecture

Separate representations serve different access patterns; this is a workload trade-off under the RUM conjecture, not a proof that read, update, and storage overhead can all be minimized simultaneously.

1. **Resident state:** Word-atomic seqlock payloads in actor arenas. One canonical arena implementation supports heap-backed testing and an engine-private Linux HugeTLB allocation path; root exports alias the persistence-wired slot/seqlock types.
2. **Durability:** Per-core WAL v2 records and CRC32C, with group-commit batching. Actual device guarantees depend on the I/O path, filesystem, drive, and power-loss behavior.
3. **Cold state:** A/B extent checkpoints and validated reloads. The new reference executor provides per-tenant bitmap/inverted/vector indexes and numeric zone maps. A separate full columnar shadow store and production search service remain open.

Previously stated 2.90× and 2.98× write amplification values are withdrawn as release guarantees. A reproducible benchmark must specify workload, record size, batch occupancy, padding mode, metadata writes, and device bytes written.

## 3. Memory and capacity

### 3.1 Revised planning budget

A 16 GiB node contains 16,384 MiB. Reserve 820 MiB, just over 5%, for allocation variability and omitted overhead instead of assigning every byte to nominal subsystems.

| Region                                                        |        MiB | Accounting requirement                                                                      |
| ------------------------------------------------------------- | ---------: | ------------------------------------------------------------------------------------------- |
| Kernel, networking, page tables, and I/O metadata             |      2,048 | Measure kernel slab, pinning, page tables and registered-buffer overhead                    |
| Agent processes                                               |      2,048 | Charge agent process memory through its cgroup; engine-created shmem remains engine-charged |
| Control heap                                                  |        512 | Include allocator metadata, stacks as assigned, bounded queues, and emergency reserve       |
| Routes, dirty lists, indexes, DMA scratch, log/export buffers |        524 | Explicitly include 33 bytes per dense dirty-list tenant slot and all label storage          |
| Engine arena                                                  |     10,432 | At most 5,216 × 2 MiB HugePages under this planning split                                   |
| Contingency reserve                                           |        820 | Do not allocate this reserve as additional steady-state actor capacity                      |
| **Total**                                                     | **16,384** | Budget model; not yet a boot-time enforced node allocator                                   |

The current lifecycle route entry stores two u64 values (16 bytes before allocator/table overhead); it does not carry the full incarnation. A future 32-byte authenticated route design would consume 30.52 MiB per million entries. The dense dirty arrays now require **33 bytes per slot**, or 31.47 MiB per million slots, excluding per-core headers. Allocate only the tenant slots owned by each core; allocating a million-slot tracker on each of 14 cores multiplies this cost by 14. The canonical full-incarnation `SlotHeader` occupies **128 bytes**. Include headers, routing metadata and external authoritative identity maps in the deployed budget.

The 10,432 MiB arena provides about 10.68 KiB per tenant if all one million are resident, before headers and fragmentation. A 10–30% resident working set is a planning assumption requiring workload validation. With 16 classes, 14 cores and one initial hugepage per class/core, 448 MiB is already assigned before payload occupancy; this is part of the arena budget, not extra capacity.

### 3.2 Sessions and labelled values

Within the nominal 154 MiB log/export portion, an example split assigns 64 MiB to at most **16,384 concurrent 4 KiB windows** and 90 MiB to other buffers. These are deployment choices, not automatically active defaults. `ExportBudget` must be shared across all sessions. Count at least two engine file descriptors per window, plus mappings and receiving-process descriptors, against host limits. Larger windows reduce the session count proportionally.

Labels have variable representation and metadata overhead. On the tested x86_64 build, `Label` is 176 bytes before up to 64 bytes of reader storage and allocator overhead; `CoreControl` is 256 bytes. The wire image header is 128 bytes, distinct from the 64-byte Rust ExtentImage value. Reproduce with `cargo run --release --offline -p ciagomem-core --example layout`. Budget actual `Label`, reader-set allocations, persisted provenance, frames, and WAL expansion. Do not assume the old 17-byte bitmap label cost applies to the new explicit-reader representation.

## 4. Concurrency and progress

### 4.1 Seqlocks and deadlines

Payload words are accessed as 8-byte atomics. Readers validate snapshots using the sequence and acquire fences; writers publish an even sequence on commit. The canonical arena uses time-bounded reads and distinguish a deadline from an observed stalled writer.

`WriterStalled` means an odd sequence stayed unchanged long enough to trigger fallback. It is **not proof of corruption or writer death**. Under release `panic = "abort"`, a writer panic terminates the process; preemption, faults and firmware pauses can delay a writer that will commit normally. Never reclaim or overwrite a slot solely on this observation.

The implemented `MonotonicClock` uses `Instant`. A calibrated TSC/CNTVCT clock, clocksource qualification, affinity assumptions and a fallback policy must be implemented and measured before claiming CPU-counter or syscall-free deadline timing. Do not infer instruction-level timing from the trait name.

For any future persistent arena attach, odd headers require repair from validated checkpoints and WAL. A private process arena has no implemented hot-attach recovery path.

### 4.2 Mesh and backpressure

P0 provides real producer/consumer SPSC lanes and doorbell coordination. The reviewed `OutboundFlow` controller is separate: it accepts a lane abstraction, tracks credits, and keeps a strictly bounded overflow queue. At capacity, `send` returns `Err(original_item)` without allocating another queue entry. The caller must reject new admissions or retain the item in another explicitly budgeted owner; silently dropping transaction-control messages is forbidden.

Credit returns acknowledge a cumulative consumed-message index. Duplicate/reordered acknowledgements are idempotent; acknowledgements beyond the sent index reject without granting capacity. `PriorityMesh` allocates separate fixed-capacity data and control meshes. A reactor must give control traffic priority; allocating the lanes alone does not provide that dispatcher.

A future reactor must drain inbound traffic before pumping outbound queues and preserve control-record progress under admission pressure. Credit conservation remains a simulation invariant to establish after integration.

### 4.3 Constant-time dirty tracking

A preallocated intrusive doubly linked list stores `prev`, `next`, `rec_lsn`, `since_ns`, and `latest_lsn`, and a checkpoint-barrier flag for dense per-core tenant slots. First-dirty insertion order follows monotonic per-core LSN order. Insert, checkpoint unlink, minimum lookup and pin decisions use a fixed number of array accesses; no heap allocation occurs after initialization.

Zero LSNs, invalid tenant indexes and decreasing mutation LSNs are rejected. `on_checkpointed(tenant, image_lsn)` rejects a checkpoint older than that tenant's latest mutation, retaining its conservative first-dirty pin. Only a covering checkpoint unlinks the node. For a hot tenant, `begin_checkpoint` freezes mutation admission and captures the latest LSN; match the existing lifecycle Flushing gate, defer writes, and complete/unlink before admitting the deferred mutations as newly dirty. `checkpoint_failed` releases the barrier while retaining the pin. These O(1) operations avoid a permanently stale hot-tenant pin at the cost of a checkpoint pause. The owner must call check_admission before assigning an LSN or appending a WAL record. on_dirty does not use the barrier to reject an already-logged record; a record logged during a checkpoint makes that completion stale, releases admission and retains the older covering pin. Append and immediate pin registration must be serialized before later appends, completion or truncation; invalid/zero/out-of-order LSNs remain errors. The head identifies the oldest required WAL record; pressure triggers a freeze-checkpoint or relocation request. No runtime benchmark threshold is substituted for these structural guarantees.

## 5. Identity, slots, and routing

`SlotHandle` packs a 40-bit generation, 4-bit class and 20-bit class-local index; zero is invalid. Generation assignment occurs within the write section. Stale handles and LSN regressions must be rejected before mutation. Durable watermarks use `fetch_max` and never move backward.

Root and module APIs now export the same canonical `SlotArena`, `SlotHandle`, `ExtentLoc`, `SeqWord`, `WordsMut`, `CoreControl` and visibility types. `ExtentLoc::new_checked` rejects overflow. `WordsMut` construction is crate-private; mutations use writer guards. Equal-LSN mutation writes reject as duplicates; checkpoint-header stamping uses a separate internal operation.

Tenant IDs use a server-held keyed PRF with canonical namespace/name collision checks. The identity journal persists names, IDs, full incarnations and deletion records. Each record chains a SHA-256 digest to the preceding head; the journal is synced before advancing an independent compare-and-set `IdentityAnchor`. Only then does the operation return success. The file lock excludes concurrent writers; write/anchor failure fences the instance. Each newly created incarnation is independently drawn as a nonzero 128-bit OS-random value and persisted in the record, rather than deterministically derived from a restorable counter. Fresh creations after joint journal/anchor restore therefore have probabilistic uniqueness assuming fresh OS entropy. This does not detect restoration of existing live history; a whole VM/RNG restore needs an independently qualified reseed protocol.

Reopening reconstructs the live identity map. `lookup` and idempotent `register` preserve an existing incarnation; duplicate `create` rejects; explicit delete/recreate obtains a fresh incarnation. Recovery requires the exact independently acknowledged journal head. Missing/corrupt acknowledged records fail closed; arbitrary torn/zero-filled bytes strictly beyond that head are truncated and synced. The test anchor is memory-only. No deployable independent anchor is bundled: its durable implementation, provisioning, backup/restore and disaster-recovery protocol are production gates. A second file restored alongside the journal does not prevent rollback.

Both the canonical slot header and **extent image format v3** retain all 128 incarnation bits. The 128-byte image header includes identity, epoch, image sequence and tombstone flag under CRC32C, plus a body checksum. Validation requires the expected incarnation, accepts image epochs no newer than the current recovered log epoch, optionally checks an exact historical epoch from trusted image metadata, and rejects tombstones. `ImageStatus` distinguishes all-zero Absent from Invalid and Valid; malformed, identity/epoch-mismatched and corrupt nonzero extents request repair, never an errno-0 fresh-tenant path. Known previous-image metadata plus erased copies also requests repair. Physical absence alone does not authorize recreating an existing catalog identity. Old v2 images fail closed; an offline migration tool is not supplied. A durable deletion/tombstone publication protocol across WAL, routes and identity anchor remains integration work; a format flag alone is not that protocol.

`nospec_index` is an architectural bounds helper. Rust bitmask syntax alone is **not** evidence of a Spectre-safe machine-code barrier. Compiler output and architecture-specific mitigation must be validated before a speculative-execution guarantee is made.

## 6. I/O, checkpoints, and the write-ahead log

The IoBackend buffer-lifetime contract for tracked lifecycle/log submissions is upheld on owner destruction by retaining backing while operations remain outstanding, rather than freeing it. Deliberate retention can exhaust memory after repeated abandonment; normal teardown must drain/cancel and reap. This is not a complete audit of all raw-buffer APIs.

The P0 workspace includes I/O traits, a deterministic seeded I/O simulator, Linux `io_uring` wrappers, WAL envelopes, group commit and a lifecycle state machine. These were absent from the original v3 embedded crate; their presence in the workspace does not imply that every reviewed API uses them.

Required lifecycle invariants:

1. Reuse only after ownership is revoked and outstanding I/O and pins reach zero.
2. Freeze buffers while the device may read them.
3. Checkpoint only state whose required WAL records are durable.
4. Write the inactive extent copy and publish the route flip only after a full successful completion.
5. Match completion operation, object and ticket so stale CQEs cannot mutate new state.
6. Verify checksums, identity incarnation and schema version when choosing a recovered image.

Component tests exercise A/B recovery and completion gating under simulated failures. Reload rejects mismatched identity/epoch/tombstone images. A corrupt selected current copy reports `NeedsRepair`/load failure instead of silently falling back. Recovery with one valid A/B copy proceeds only with trusted recovered WAL coverage: `wal_start_lsn - 1 <= checkpoint_lsn <= wal_end_lsn`. Otherwise it requests repair. A nonempty suffix emits ReplayRequired and keeps the route unpublished until the trusted driver applies/scans the complete committed suffix and calls finish_replay. Abort revokes and frees a failed or deleted replay; incomplete replay never publishes a client route. A no-suffix checkpoint can publish directly. Both valid copies can be ranked by image sequence. The production WAL decoder, catalog continuity, deletion replay and repair service remain integration work; a caller-supplied interval must be established by recovery rather than guessed. Normal clean cold loads instead use the tenant-specific empty interval RecoveryBounds::clean(current_epoch, image_epoch, checkpoint_lsn) from durable metadata proving no uncheckpointed tenant mutations. Exactly that image loads without a global suffix scan; stale images and dirty tenants with missing WAL still request repair. The WAL exclusively claims its core controls at open and publishes the recovered end after superblock_durable confirms the reopened superblock. Publication remains monotonic, so open rejects a reused control whose watermark exceeds the recovered end. Recovering a shorter prefix requires fresh controls and arenas plus explicit loss/repair handling; fresh memory alone does not resolve lost acknowledged data. There is no manual new-commit prerequisite.

On terminal WAL write failure, pending acknowledgements become **indeterminate**, the log and core fence, later CQEs cannot advance published durability. Fence and durable state live in a node-owned Arc array explicitly shared by every size-class arena and WAL for that core. Fields are private and publication/fencing serialize on a control-path mutex. Reads check only their validated owner core and recheck the fence after snapshot validation. Other cores remain available. Allocate the control array once per node. WAL open binds the array/core ID and holds the core's only DurabilityPublisher. No external arena holder can directly advance the watermark or revoke its claim. Duplicate binding rejects and CQE processing cannot substitute another control object. The guard also releases the claim if initialization fails; invalid/exhausted recovery counters reject rather than wrapping. The host must call log.validate_arena for every class before accepting work; mismatched allocations reject. There is still no node constructor to enforce complete registration automatically. This does not erase bytes a device may already have persisted: recovery must resolve the outcome. Stale/duplicate lifecycle CQEs are rejected before decrementing in-flight counts. Lifecycle submissions provisionally increment an arena pending counter before calling the backend; rejected submissions roll it back, and genuine completions decrement it. WAL buffers similarly enter InFlight before backend code can unwind. Both on_cqe handlers are unsafe: the caller must prove that a matching token/result is a completed request owned by that controller, with no remaining device access. Proven nonmatching stale tokens do not release active buffers. A pending-I/O lifecycle/log drop fences the core, and the arena/log retains backing referenced by abandoned operations until process exit. The owning destructors enforce this tracked drop-path lifetime; caller discipline alone is insufficient. The production reactor must cancel and reap every operation before reclaiming memory. release_device_references is an unsafe acknowledgement that prior device references have terminated; subsequent submissions invalidate that acknowledgement.

The pin tracker supplies an explicit checkpoint admission barrier, and lifecycle Flushing defers mutations. Coordinating begin, submit, CQE completion/failure, durable seal, relocation and admission resumption still requires the future reactor. Reserve control-record headroom independently from user-log capacity to prevent a full ring from blocking completion records.

## 7. Transactions and recovery

The corrected `VoteTable` is owner-local state. `open`, `vote`, and `close` require exclusive mutable access. A ballot includes transaction sequence and participant index; stale and invalid ballots are rejected; matching duplicates and conflicting duplicates are distinguished before mutating either vote mask. Transaction sequences must advance and may not be reused. Participant 63 is an ordinary bit, and all 64 participants must vote before a 64-participant decision.

This removes the tag-check/atomic-RMW ABA window. The table does not supply a sequencer, deterministic scheduler, lock manager, network protocol or replay engine.

A complete Calvin implementation must:

- Sequence transactions deterministically and carry `tx_seq` through every participant record.
- Persist a unique decision and participant data before the applicable acknowledgement boundary.
- Recover by first collecting durable decisions and then applying only authorized committed prepares.
- Wait for all required participant durable watermarks and, in strict HA mode, replica acknowledgements.
- Reject cross-tenant participants unless explicitly authorized by the session and ownership policy.

**Strict ACID and strict serializability remain unproven for an integrated engine.** Component voting and single-log recovery tests cannot establish those properties.

## 8. Authorization, capabilities, and exports

### 8.1 Session and query authorization

The trusted authentication path verifies a signed capability for the current tenant incarnation, UID, session, audience, boot epoch, operation, key epoch and time. Only verification constructs `VerifiedMacaroon`; callers cannot fill its fields directly. Re-verify for every request so expiration and key-epoch changes apply. The reference engine additionally owns a boot epoch and trusted clock, rejecting expired/foreign-boot verified tokens at each public Engine entry; its clock high-water mark prevents regression from reviving a connection token. The engine remains Send + Sync when its Journal does; its clock requires Send + Sync and its high-water time is atomic. LocalExportRoot is separately bound to a boot epoch and requires trusted current time for each create, rejecting expired/foreign-boot tokens before creating a file. Pure helpers such as join::equi_join process caller-held snapshots and are not independent authentication boundaries. This is entry-time validity, not cancellation of an in-flight query or revocation of an already returned file. `AuthContext` also receives an authoritative actor-owner resolver and operator-maintained destination rules.

A single-actor source, every transaction participant, and all typed qualified actor paths must belong to the verified tenant. Operator scope enables only scans within the verified tenant. Fleet-wide backup rejects until a separately rooted fleet capability and adapter exist. Writes require write permission; an operator bit does not silently waive actor ownership. Policy is required even for ASTs supplied programmatically. Successful policy produces an opaque `ValidatedQuery` carrying tenant/session and owner version. The reference executor rejects a stale owner version before execution; its exclusive owner operation prevents an ownership change during execution. External ownership catalogs require their own consistent version/epoch protocol.

### 8.2 Cryptographic primitives

`ciagomem-crypto` supplies RustCrypto HMAC-SHA256 behind `Prf`, domain-length framing, MAC chaining and constant-time equality through `subtle`. The earlier ARX function is removed from the default implementation. An RFC 4231 known-answer test exercises HMAC-SHA256.

Macaroon verification requires signed Tenant, ExpiresAt, PeerUid, Session, Audience, BootEpoch, Ops and KeyEpoch caveats. Generate SessionEpoch once per engine startup from OS entropy, retain it outside restored data, and populate verification context from this trusted value, never from request input. All caveats are conjunctive; attenuation cannot add authority. Unknown, missing, expired, mismatched, or unsigned caveats fail closed.

Reference MACs bind tenant incarnation, session, actor, expiry, opaque ID and trusted vault location/generation/digest metadata. `$ref:0xID.32hex` preserves the 128-bit MAC in the AST; bare IDs reject. Resolve against the authenticated tenant's private index. Missing, forged and expired references return the same denial class. This does not claim constant-time index lookup. Digest generation, storage, secure root-key provisioning, authenticated transport and key rotation are integration responsibilities; no production vault service is claimed.

### 8.3 Export isolation

The engine arena remains private. An agent receives only an authorized projection through a sealed read-only memfd window. The implemented constructor creates mappings, sets shrink/grow/future-write/seal seals, verifies denial of both a new writable shared mapping and `mprotect` upgrade on a read mapping made from its **O_RDWR** descriptor, then reopens the agent descriptor O_RDONLY. An O_RDONLY-only probe would be vacuous. Owned descriptors/mappings/budget reservations use RAII; only a borrowed agent descriptor is exposed. The relevant kernel fix is commit `8ec396d05d1b`; the earlier rejected CVE identifier is withdrawn. This probes the required behavior on the running kernel; it is not a general kernel security certification.

`ExportBudget` reserves bytes and a session count before allocation and releases them on failure or drop. The session manager must use one shared authoritative budget. FD passing through `SCM_RIGHTS`, projection publication, seqlocked cross-process access, request rings and session lifecycle are not implemented as a service. Publish lazily on a session request rather than copying K windows on every mutation unless a measured budget permits it.

## 9. Information flow and AI frames

Integrity is ordered `External < AgentDerived < UserDirect < Policy`. Joining inputs takes minimum integrity, union of provenance category bits and intersection of explicit reader principal IDs. Reader IDs are 128-bit values, not `tenant % 64` bits. The current bounded representation accepts at most four distinct readers and rejects larger sets.

Frame admission requires Policy integrity for hard constraints, UserDirect for active intent/playbooks, AgentDerived for facts, and permits External only in untrusted variable positions. The runtime must populate engine-attested variables itself and render untrusted content outside trusted instruction prefixes.

Labels have private fields; public untrusted construction produces only External integrity. Higher integrity requires verified **Ed25519 content-bound provenance** from a configured trusted public key. Derived/P-LLM labels clear issuer and signed scope; a recomputed digest is not an attestation and derived bytes require fresh signed provenance before Fact admission. A verifier contains no signing material. An endorsement binds tenant, session, engine boot epoch, exact source categories, content digest, destination frame field, tool, sink, expiry and a nonzero nonce. Per-tenant quotas and a global active-tenant bound isolate replay capacity; a min-heap expires entries without a full-map scan. Approvals with more than 300 seconds remaining reject; accepted approvals retain replay protection until their actual expiry on the tracker's monotonic high-water clock. Admission evaluates stale timestamps at that clock for session, provenance and approval checks; client-provided time is not permitted. Mandatory boot binding in capabilities, provenance, approvals, tracker and frame permits prevents old approvals crossing restart even if a logical session ID is reused. Zero-source approvals reject. A valid approval may elevate AgentDerived intent/playbook content; it never elevates External content or manufactures Policy authority. Signing keys belong only to the authenticated trusted issuer.

P-LLM output inherits the join of **every** context label and is capped at AgentDerived integrity. Q-LLM output is always External. Empty context defaults to External with no readers. Frame admission checks the reader set before admission and checks trusted provenance against the supplied bytes. Labels alone never authorize tool side effects. Frame admission rejects expired authenticated sessions; action permits expire no later than the issuing capability, even if provenance and approval last longer. `authorize_tool_call` consumes a one-use frame permit bound to the same session, tool, sink and argument digest, with expiry and reader checks. Retain one replay tracker for the boot/session lifetime; resetting it mid-boot is unsupported. Generate a fresh OS-random SessionEpoch after every engine start and obtain newly issued capabilities, provenance and approvals. Reusing a fixture boot epoch, or rolling back a running engine and its RNG without a reseed protocol, defeats this premise.

These helpers must be invoked by every frame compiler, executor and gateway path. A standalone correct lattice does not establish end-to-end prompt-injection resistance.

## 10. CiaQL grammar and policy

The default API uses the P0 token stream, recursive-descent parser and typed AST, extended for shorthand and named arguments. It does not reuse the ZIP's raw substring-stage parser. The maximum query length is 64 KiB, with 16,384 tokens and independent stage, nesting, list and transaction participant bounds. Similarity vectors have a separate 4,096-dimension bound, rather than the generic 256-element list bound. Unknown calls reject. IN/CONTAINS are case-insensitive. Iterative preflight bounds caller-built ASTs before recursive semantic walks; Query destruction drains recursive nodes iteratively. Callers must validate before using recursive Clone/Debug/PartialEq on an untrusted programmatically constructed AST.

Implemented sources are actor IDs (`@id` or `ACTOR(id)`), `TX_ACTORS[...]` / `tx [@id,...]`, scans, and operator-policy-checked backup destinations. Implemented stages include comparisons, boolean expressions, mutations, projection, grouping/reduction, numeric-vector similarity, bounded search arguments, frame stages, TAKE and EXPORT.

```text
@0x0008F421
where state.status == "ACTIVE" and state.wallet.balance_cents >= 5000
mutate {
  state.wallet.balance_cents -= 5000,
  state.metrics.tx_count += 1
}
select {
  remaining_cents: state.wallet.balance_cents,
  tx_lsn: meta.last_tx_seq
}
take 5
```

`|>` may separate stages without changing their meaning. Strings are tokens, so a string containing `top_k: 7`, `limit`, or `|>` cannot supply an argument or split a stage. Bare field `limit > 3` is parsed as an implicit state field. Qualified paths retain explicit roots. Unknown stages, malformed values, duplicate arguments, duplicate participants and trailing garbage are errors.

TAKE must be final except for one following final EXPORT. Search `top_k` defaults to 10 only when omitted; an explicitly invalid value is an error. Similarity preserves numeric vectors and metric; runtime vector parameters such as `$q` fail until binding is implemented. Named search fields remain restricted to a field path, not an executable expression.

Parsing does not execute queries or authorize them. Policy validates the resulting AST and requires exact destination matches: S3 rules name a concrete bucket plus canonical path prefix; local rules are under `/var/backups/ciagomem/`. Reject ambiguous escaping, userinfo, query/fragment components, traversal, broad scheme-only rules and path-prefix boundary confusion.

The `ciagomem-engine` reference crate now executes typed reference joins, programmatic bounded equijoins, registered referential links, strict and chunked cascades, projected/schema upgrades, hybrid search, and grouped aggregates. It uses explicit bounded selection rules rather than a general cost-based join optimizer. Tenant-name aliases and unsupported administration syntax remain errors. The following sections specify these capabilities and distinguish the reference implementation from production reactor/WAL integration.

This document specifies the five database capabilities in the Ciagomem ecosystem and records their implementation boundaries. The accompanying `ciagomem-engine` crate is an executable, owner-local reference engine. It composes typed CiaQL, verified sessions, per-tenant indexes, immutable snapshots, and journal-gated writes. It does not start a network server or pin operating-system threads.

### 10.1. Architecture and invariants

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

### 10.2. Joins: reference lookup and bounded read fan-out

#### Direct relationship path

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

#### Cross-core protocol

`join::Request` carries a unique ticket, authenticated tenant context, full target reference and deadline. `join::Fanout` tracks at most a configured number of outstanding targets. Replies must match the ticket and actor reference; a successful snapshot must match the tenant and reader permission. Wrong-target responses cannot consume the valid request. Expiration releases pending state; late and duplicate replies fail.

The integration suite sends these requests and replies over the existing real `Mesh<Request, N>` and `Mesh<Reply, N>` Lamport SPSC lanes and verifies that the returned record is the same `Arc` as the owner snapshot. Endpoints remain single-producer/single-consumer. A full lane returns the unsent message; the dispatcher must retry within a bounded budget or reject admission. It must never spin until a peer drains.

The direct executor currently resolves cross-shard reads synchronously in-process. The ticket protocol and actual mesh transport are tested together, but a continuously running asynchronous reactor dispatcher is not supplied. A reference-engine call holds exclusive engine access; mesh replies independently provide per-actor committed snapshots, not a common distributed snapshot epoch. An integrated deployment needs an epoch or version-validation rule for stronger join consistency.

#### General equijoin path

`join::equi_join` handles relationships without direct references. It builds a bounded ordered key map for one input, probes the other, preserves duplicate matches, skips missing/null keys, verifies every input tenant/label, joins provenance, and returns pinned snapshots. Input work, build-key bytes, result rows and descriptor bytes are bounded.

Its keys use explicit scalar types, like grouping. It does not promise SQL implicit casts. This API is programmatic; the CiaQL JOIN syntax currently selects the actor-reference path. Arbitrary predicate joins, distributed repartition joins and unbounded Cartesian products are not implemented. They require explicit work limits and spill/admission semantics rather than an assumption that mesh messaging makes them cheap.

Read-only fan-out is **not a saga**. Sagas describe multi-step writes and compensation or fail-forward cleanup, not the consistency of a read join.

### 10.3. Referential integrity, RESTRICT and cascades

An explicit edge records `dependent ActorRef → target ActorRef` and an `OnDelete` policy. `Links` stores incoming edges for efficient delete planning and outgoing edges for cleanup. Both endpoints must exist in the same authorized tenant at link creation. The total edge population is capped. A plain reference field does not automatically register a constraint; applications must create the edge.

```text
@2 link add(target: @1, on_delete: RESTRICT)
@3 link add(target: @1, on_delete: CASCADE)
```

Deletion walks incoming CASCADE edges with a visited set. Cycles terminate. It rejects an oversized closure and checks RESTRICT dependencies outside the proposed closure. Validation completes before any deletion record is committed.

#### Strict owner-coordinated deletion

`Engine::delete` journals one batch containing the complete bounded closure, then removes rows, bitmap membership, text postings, vector entries and links. A journal error leaves the previous visible state unchanged. This provides atomic visibility within the reference engine's exclusive owner operation. Durable, cross-reactor atomicity still requires the integrated Calvin/WAL protocol; an in-memory batch is not proof of distributed ACID.

#### Explicit asynchronous cleanup

`begin_delete` journals the cascade plan and marks every actor in its closure as deleting. Such actors disappear from scans and cannot accept new links or mutations. `advance_delete(job, expected_cursor, max_rows, session)` journals a bounded chunk plus cursor progress before physically reclaiming it. Repeated/stale cursor calls are rejected rather than deleting another chunk accidentally. The caller can inspect the authorized status and resume with the current cursor. Job/plan memory shares a fixed closure budget.

This is a **fail-forward deletion saga**. It never compensates by resurrecting deleted data. Logical deletion is established before incremental reclamation; physical cleanup is eventual, not instant. Journal adapters and restart recovery must persist/replay the plan, tombstones and progress together. The supplied MemoryJournal records them in process but does not recover them after process loss.

These mechanisms move integrity checks to actor-owned metadata. They do not eliminate integrity-checking overhead, and unregistered asynchronous hooks are not a substitute for constraints.

### 10.4. Verified FlatBuffers and lazy schema evolution

`schemas/actor.fbs` defines the physical actor image with explicit identity, tenant, application schema version, scalar fields, provenance/readers and an optional embedding. Code is generated by upstream `flatc 25.2.10`; generated bindings are committed so normal builds need no compiler installation.

`image::encode` constructs an immutable buffer. `image::view` checks the identifier and verifies the root before exposing borrowed accessors; it also checks paired reader-vector lengths. Only this verified path is used by the handwritten code. The private generated module contains upstream unsafe implementation details and is isolated from handwritten safe execution code. Verification does not authorize a buffer or authenticate its contents.

Application schemas advance through an ordered registry of `fn(&Fields) -> Result<Fields>` transitions. The reference registry describes one schema family per Engine. Registering version N→N+1 requires N to be current. Every transition must be deterministic and total over valid input, and bounded by the actor budget. Those are application contracts to test; Rust function pointers alone do not prove purity.

`Engine::projected` returns a current-schema view while the stored version remains untouched. Query execution does the same for old-schema actors. `Engine::upgrade` is an explicit owner write: derive the new fields, preserve identity/labels/embedding, validate and encode the result, journal it, then publish it and refresh indexes. No reader performs an in-place rewrite or changes a seqlock sequence.

A projection-only read can allocate for the transformed map; it is not a universally zero-copy operation. Avoid eager fleet-wide rewrites. New fields may receive defaults through projections. Physical FlatBuffers evolution must follow upstream rules: append fields or use explicit stable IDs, deprecate rather than remove, and check compatibility with `flatc --conform` in a schema-release pipeline. Do not change field types/defaults casually.

The P0 A/B checkpoint format has not been replaced by this image format. Versioned WAL/checkpoint records, durable schema registration, multi-family catalogs and restart replay must be integrated before declaring live production schema migration complete. Indexed search is conservatively rejected after advancing the global schema registry until a version-consistent index migration path exists; it cannot silently use stale postings against projected data.

### 10.5. Text search, SQ8 vectors and hybrid ranking

#### Text index

Each tenant/core namespace has field-specific inverted postings `(field, term) → (slot, term_frequency)` and document lengths. The initial tokenizer splits on non-alphanumeric characters and lowercases Unicode text. It does not provide stemming, stopwords, phrase/proximity search or language-specific analyzers.

Insertion and replacement update current postings; deletion removes membership and empty containers. The current implementation uses mutable owner-local postings, not an append-only segment/compaction engine. A future log-structured index must carry actor incarnations/versions and tombstone stale postings.

BM25 uses k1=1.2 and b=0.75. Corpus size, mean document length and document frequency are calculated from the authorized candidate set at that stage. This gives explicit filtered-corpus ranking and avoids cross-tenant statistics. Local scores are combined as candidates; a production global-BM25 mode needs authorized global statistics. Deterministic tie-breaking uses actor IDs.

#### SQ8 and SIMD

Each vector retains its full-precision source plus symmetric signed 8-bit codes with scale `max(abs(x))/127`, protected against zero/subnormal scale. Values/dimensions are validated at ingestion. Eight i32 lanes accumulate quantized dot products through `wide::i32x8`, with a scalar tail. The 4,096-dimensional bound keeps integer accumulation within i32 range. SIMD instruction selection depends on compiler target capabilities and may use portable fallback paths.

Exact execution is the default: cosine, dot and Euclidean distance use retained full-precision values, and thresholds are checked after scoring. L2 accepts distance ≤ threshold; cosine/dot accept score ≥ threshold. Zero-vector cosine is defined as zero.

Explicit `Options { approximate_candidates: Some(K), ..Default::default() }` uses SQ8 cosine to shortlist K candidates and reranks them with full precision. It reports `stats.approximate=true`. Approximate shortlisting can lose recall: it is never presented as an exact predicate. The current shortlist enumerates candidates; it is not an ANN graph index. Keeping both representations costs more than codes alone and is included in accounting.

#### One CiaQL pipeline

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

### 10.6. Scans, bitmaps, zone maps and analytics

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

### 10.7. Commit and recovery boundary

`Journal::commit(Batch)` receives validated Put/Delete/Link/BeginDelete/DeleteProgress records. A production adapter must durably append a complete recoverable batch before returning success. Queries finish downstream evaluation and output-budget checks before committing pending mutations. An error or refused journal batch leaves prior published actor/index state unchanged.

`MemoryJournal` is deliberately non-durable and bounded. It makes these transitions observable in tests; it is not a WAL adapter, crash replay engine or ACID certification. Integrating the P0 group-commit log requires a versioned batch encoding, prepare/decision boundaries, completion-driven publication, metadata recovery and explicit durable client acknowledgements. Returning success from the reference engine means its configured journal accepted the batch, not that an NVMe or replica persisted it.

### 10.8. Memory and performance contracts

`Limits` bounds actors per core, estimated owner bytes, query work, result bytes, groups, joins, fields, text, vector dimensions and cascade closure size. Record images, typed shadows, index estimates and retained journal images all consume memory; they are not free because a payload can be borrowed.

Accounted bytes are conservative logical estimates, not an allocator-enforced physical RAM quota. Pinned snapshots retained by external callers, BTree allocator details, VMAs, kernel buffers and the server session budget require separate accounting. A production engine must charge outstanding snapshot lifetimes to the request/session and integrate these limits with the 16 GiB node budget.

Index memory trades against the resident working set. Index only fields justified by workload selectivity in a production catalog; the reference indexes all scalar/text fields for functional coverage. Do not extrapolate the example dataset directly to one million users. The reference mesh demonstration does not copy actor images, but projection, tokenization, image construction, index maintenance and schema conversion still cost work.

Run the reproducible local comparison:

```sh
cargo run --release --offline -p ciagomem-engine --example benchmark
```

It reports snapshot-handle time, accounted bytes and identical selective queries with and without bitmap pushdown over 10,000 records and four simulated shards. The source, dataset and query are committed. It does not compare against PostgreSQL, MySQL, Redis, a vector database, or another AI-memory system. The host is not an isolated benchmark runner; outputs are observations, not release guarantees.

### 10.9. Open-source components and provenance

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

### 10.10. Verification and remaining integration

`tests/ecosystem.rs` covers joins across actual SPSC lanes, snapshot ownership, foreign/stale references, alias policy, strict and chunked cascades, cycles and retries, journal failure, schema projection/publication, borrowed FlatBuffers access, index/scalar equivalence including numeric limits, zone refresh and slot reuse, text update/delete, hybrid exact/approximate modes, SIMD tails, grouping, and admission failures.

Still required for a deployed database: a pinned asynchronous reactor dispatcher; durable WAL encoding/replay for all new records; persistent schema/index/link catalogs; multi-reactor transaction epochs; cold-I/O suspension; index reindexing across schemas; authorized request transport; complete memory accounting; client SDK/server interfaces; cross-system workload benchmarks; and hardware/failure qualification. Those gaps do not remove the five features from product scope, but they prevent representing this component milestone as a completed production system.

The mesh consumer refreshes a cached tail before clearing pending work after a new doorbell; a deterministic regression guards against stranding the final published batch.

## 11. Operations, backup, and evolution

### 11.1 Durability contracts

Local durable commit, synchronous replicated commit and asynchronous archive are different contracts. RPO=0 across node loss requires synchronous replicated acknowledgement and tested failover; asynchronous S3 archival has RPO bounded by observed archive lag. No standby, spool, archive, encryption, backup or PITR service is implemented here.

A future backup engine must define a transaction-consistent epoch cut, preserve identity allocation history, protect per-tenant DEKs through KMS, and restore linked tenant history consistently. Tenant-only PITR with cross-actor history must fork to a new incarnation unless a validated coordinated rewind is available.

### 11.2 Host qualification

The future boot path must validate memory/FD budgets, page availability, clocksource behavior, CPU isolation, I/O privileges and export-window tests before accepting traffic. CPU quotas, SMT sharing and interrupt placement must be part of the measured deployment profile. Values such as `isolcpus` ranges and HugePage counts are hardware-specific configuration, not universal commands to copy into an unqualified host.

### 11.3 Advanced components

- **Shard migration:** Phase 3. Sequence migration with transactions; log source/destination LSN mappings in both logs; gate route flip on destination durability; preserve incarnation and vault access; use dwell-time/load hysteresis.
- **Rolling upgrades:** Prefer side-by-side checkpoint/WAL recovery and tenant migration. A named shared HugeTLB arena requires ABI compatibility, access control, header repair and validated replay; no zero-downtime attach guarantee is made.
- **eBPF:** Complement in-process semantic counters with sampled kernel tracepoints/fentry from a separate privileged loader. No “zero overhead” claim; do not place uprobes on read/commit hot paths without a measured budget.
- **Schema upcasting:** Phase 2. Log deterministic owner-side upgrades; retain schema versions and labels in A/B images. Readers project old layouts and enqueue upgrades rather than writing through read paths.
- **Geo-replication:** Pin actors to a home region. Cross-region transactions pay sequencing/consensus latency; asynchronous followers expose explicitly bounded staleness. Concurrent writes to the same actor require a stated WAN-consensus or weaker semantic contract.

## 12. Workspace and reference implementation

The current dependency DAG is:

```text
ciagomem-ql ──────────┐
                     ├── ciagomem-core ── ciagomem-wal
ciagomem-crypto ──────┘          │
                               └── ciagomem-engine ← roaring / flatbuffers / wide
                                    (also depends directly on ciagomem-ql)
```

The parser has no storage, cryptography or I/O dependency. Only the crypto crate directly names hash/MAC dependencies. Workspace crates use the canonical `ciagomem-*` names, Rust imports use `ciagomem_*`, and the CiaQL parser module is `ciaql` (`ciaql.rs`). Consumers of older revisions must update their dependency names and imports.

Core compiles the dependency-free envelope source physically located in the WAL source directory, and WAL re-exports it. The Cargo graph is acyclic, but this source-layout coupling must be removed in the future types/format crate extraction. P0 arena/I/O code still contains `libc` operations; the final syscall and unsafe-code partition is not yet achieved.

The vulnerable v2.5 API is under `ciagomem_core::legacy` and is compiled only with the nondefault `legacy-audit` feature. Its tests intentionally reproduce defects. It is not the default authorization path. Original ZIP code and adversarial assertions are preserved as text in `review/v30` for provenance.

Build from `ciagomem-slae/` with Rust **1.89 or newer** (standard-library file locking is required; the available 1.99.1 toolchain is the tested toolchain):

```sh
cargo test --release
cargo test --release --all-features
python3 scripts/validate_workspace.py
```

Cryptographic and query-engine dependencies are vendored from unmodified upstream crate archives for offline builds; their licenses and archive SHA256 hashes are retained. Registry availability is not needed for these added dependencies. Existing libc/io-uring dependencies must still be available locally or fetched normally.

### 12.1 Source of truth

The crate manifests, modules and tests in the workspace are authoritative:

- [Core manifest](ciagomem-core/Cargo.toml), [security](ciagomem-core/src/security.rs), [policy](ciagomem-core/src/policy.rs), [dirty list](ciagomem-core/src/dirty.rs), [votes](ciagomem-core/src/calvin.rs).
- [Typed parser](ciagomem-ql/src/ciaql.rs), [cryptographic capabilities](ciagomem-crypto/src/lib.rs).
- [Lifecycle](ciagomem-core/src/lifecycle.rs), [group commit](ciagomem-wal/src/gclog.rs), [envelopes](ciagomem-wal/src/envelope.rs).
- [Corrected review regressions](tests/v30_regressions.rs).
- [Actor-native runtime](ciagomem-engine/src/runtime.rs), [execution tests](ciagomem-engine/tests/ecosystem.rs), and [detailed ecosystem design](docs/ACTOR_NATIVE_QUERY_ENGINE.md).

### 12.2 Core library listing

This listing is synchronized from `ciagomem-core/src/lib.rs`. It is a **workspace module**, not a self-contained crate: companion modules and Cargo dependencies above are required. The validator checks exact source equality so prose and implementation cannot silently diverge.

<!-- BEGIN REVIEWED CORE SOURCE -->

```rust
//! Ciagomem component kernel. Canonical storage types are the persistence-wired P0 family.
#![allow(clippy::too_many_arguments)]
pub use calvin::{Participants, Vote, VoteTable};
pub use seqlock::{
    READ_RETRY_LIMIT, ReadError, SeqWord, SeqWriteGuard, WordView, WordsMut, WriterBusy,
    read_deadline, read_with,
};
pub use slot::{
    CoreControl, ExtentLoc, SlotArena, SlotHandle, SlotHeader, SlotReadError, Snapshot, TenantWord,
    Visibility,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineReadError {
    DeadlineExhausted { retries: u32 },
    WriterStalled { odd_seq: u64 },
    Corrupt,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    WriterBusy,
    Reassigned,
    LsnRegression { current: u64, attempted: u64 },
    DuplicateLsn,
}
pub trait Clock {
    fn now_ns(&self) -> u64;
}
pub struct MonotonicClock;
impl Clock for MonotonicClock {
    fn now_ns(&self) -> u64 {
        static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        EPOCH
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos()
            .min(u64::MAX as u128) as u64
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    None,
    FreezeCheckpoint { tenant: u32 },
    Relocate { tenant: u32 },
}
pub mod dirty;
pub use dirty::TruncationPins;
pub mod flow;
pub use flow::*;
pub mod security;
pub use ciagomem_crypto::*;
pub use security::*;
pub mod policy;
pub use ciagomem_ql::{ParseError, Query as CiaQuery, Source, Stage, parse as parse_ciaql};
pub use policy::*;
pub mod calvin;
#[path = "../../ciagomem-wal/src/envelope.rs"]
pub mod envelope;
#[cfg(target_os = "linux")]
pub mod export_window;
pub mod io;
#[cfg(feature = "legacy-audit")]
pub mod legacy;
pub mod lifecycle;
pub mod mesh;
pub mod seqlock;
pub mod slot;
#[cfg(target_os = "linux")]
pub mod uring;
#[derive(Debug, Default)]
#[repr(C, align(128))]
pub struct CachePadded<T>(pub T);
impl<T> core::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

#[cfg(target_os = "linux")]
pub mod destination;
```

<!-- END REVIEWED CORE SOURCE -->

## 13. Verification and production exit criteria

### 13.1 Reproducible component checks

Use the workspace commands in Section 12. The default suite exercises current component regressions; the all-features suite additionally exercises deliberately vulnerable historical APIs. `v30_regressions`, `r5_regressions` and `r6_regressions` replace supplied defect assertions with tests of the corrected contracts, including negative authorization cases and successful permitted operations.

The known-answer test verifies HMAC-SHA256 against RFC 4231. Journal tests check stable identity across restart, fresh incarnation after explicit recreation, lock conflicts, rollback detection, acknowledged-prefix corruption rejection and safe unacknowledged-tail truncation. Deadline tests use a deterministic injected clock for stall semantics, avoiding sleeps as a claim about writer death. Dirty-list tests exercise a 70,000-slot list without using CI wall-clock timing as a complexity oracle. The seqlock stress test validates snapshots with a synchronized start and finite work rather than requiring a machine-dependent throughput count.

The workspace validator checks the Cargo DAG, parser and crypto dependency boundaries, opt-in legacy feature, local documentation links and exact equality of the embedded core listing. This is not the final full-DAG/syscall isolation gate proposed by the review.

### 13.2 Required validation before production

| Layer                    | Required evidence                                                            | Current disposition                                                                                                                                                                                                                                                                                                                                                                     |
| ------------------------ | ---------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Static analysis          | Scoped unsafe audit, dependency/license audit and no-warning production gate | Formatting and ordinary Clippy available; P0/upstream warnings remain                                                                                                                                                                                                                                                                                                                   |
| Property tests           | Lattice laws, dirty-list oracle, generation packing and credit conservation  | Targeted regressions exist; large property campaigns remain                                                                                                                                                                                                                                                                                                                             |
| UB/concurrency           | Miri, loom and randomized schedule exploration on the actual implementation  | Not run locally for current R9 changes. The R9 reviewer reports selected acceptance Miri runs (intentional-retention cases allow leaks), full core-library Miri without leak suppression, and an aarch64 compile check on its supplied patch. These do not cover our added release-flag regression fix; engine Miri, loom/Shuttle, ARM execution and real io_uring teardown remain open |
| Parser/security fuzzing  | Structured grammar fuzzing and tenant-owner oracle checks                    | Deterministic examples only; long campaigns remain                                                                                                                                                                                                                                                                                                                                      |
| Deterministic simulation | Integrated WAL/transaction/reactor with seeded clock/network/storage         | P0 storage simulation exists; whole-engine DST is absent                                                                                                                                                                                                                                                                                                                                |
| Crash and power loss     | All submit/completion/fence crash points and real NVMe cuts                  | Simulated P0 recovery tests only                                                                                                                                                                                                                                                                                                                                                        |
| Transaction isolation    | Strict-serializable histories under failure and failover                     | No integrated transaction service to qualify                                                                                                                                                                                                                                                                                                                                            |
| Formal analysis          | Transaction, migration and extent protocols with documented refinement       | Design gate only                                                                                                                                                                                                                                                                                                                                                                        |
| Hardware performance     | Isolated-core histograms and sustained bounded memory                        | No calibrated release benchmark or node-scale evidence                                                                                                                                                                                                                                                                                                                                  |

### 13.3 Per-path performance objectives

These are **unmeasured release objectives**, not results for this workspace:

| Path                                 | Qualification objective     | Scope                                                                               |
| ------------------------------------ | --------------------------- | ----------------------------------------------------------------------------------- |
| Resident in-engine same-core read    | p99 ≤ 200 ns; p99.99 ≤ 1 µs | Uncontended validated snapshot, no authorization/network/LLM round trip implied     |
| Already-published agent window read  | p99 ≤ 300 ns                | Publication age measured separately; excludes request transport and copy            |
| Single-actor durable commit          | p99 ≤ 50 µs                 | Qualified PLP NVMe and explicit local/replicated durability contract                |
| Cold actor fault-in                  | p99 ≤ 250 µs                | Qualified device, queue depth, validation and load policy                           |
| Cross-core or cross-region operation | Separately measured         | Mesh, epoch and WAN consensus latency cannot be hidden inside the local-read target |

“Zero-token waste” and the earlier unsupported token-reduction percentage are withdrawn. Measure tokens per completed task against a published baseline corpus, including frame retrieval and tool outputs.

### 13.4 Observability requirements

Export per-core counters or histograms for retries/deadline exhaustion/stalled writers, combined reads, lane occupancy, overflow rejection, excess credits, log free bytes, oldest pin age, freeze/relocation actions, generation headroom, authorization denial, bad MAC, identity collisions, export-budget usage, cgroup throttling and archive/standby lag. These are a required catalog; there is no Prometheus endpoint in the workspace.

Semantic metrics should use bounded in-process state; kernel instrumentation is complementary and its cost must be measured. Full production sign-off requires closure of the remediation ledger, security-path integration, verified recovery/replication, host resource enforcement and measured SLOs. Passing component tests does not waive these gates.
