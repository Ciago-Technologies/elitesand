# R5 review disposition — 31 October 2026

> Historical ledger. The [R9 ledger](R9_REMEDIATION.md) supersedes publisher access and pending-device-I/O teardown contracts.


> Historical ledger. The [R8 ledger](R8_REMEDIATION.md) supersedes durability binding, local export freshness, Engine thread traits and current cleanup/policy layout.


> Historical ledger. The [R7 ledger](R7_REMEDIATION.md) supersedes admission/pin ordering, clean-load bounds, security time, engine token freshness and WAL control/recovery publication contracts.


> Historical ledger. The [R6 remediation ledger](R6_REMEDIATION.md) supersedes shared core controls, derived-label authority, random incarnations, replay quotas/boot binding, recovery epochs and checkpoint barriers.


The supplied **Ciagomem-SLAE v3.0 Remediation Deep-Research Review** and `ciagomem-r5-review-harness.zip` analyze the older four-crate upload. The current workspace also includes the actor-native reference executor. We retained that executor and applied the relevant fixes to its dependencies and callers. We did not overwrite current sources with the older archive.

Original core/WAL defect assertions and the mesh patch are preserved under [review/r5](../review/r5). Several original tests pass when a vulnerability exists. Corrected positive and negative contracts live in [r5_regressions.rs](../tests/r5_regressions.rs), [v30_regressions.rs](../tests/v30_regressions.rs), [WAL regressions](../ciagomem-wal/tests/r5_wal.rs) and module tests. Passing historical reproductions is not security sign-off.

“Component fixed” below means the supplied component contract changed and has executable regression coverage. It does not mean the deployed system is complete. This is a breaking revision: callers, identity metadata, reference literals, labels and checkpoint images changed.

| Finding | Current disposition and evidence | Remaining boundary |
| --- | --- | --- |
| R5-M1 | Component fixed before this report arrived: refresh the published producer tail before clearing pending work. Deterministic cached-tail regression and concurrent FIFO test. | Weak-memory model checking remains open. |
| R5-D1 | Component fixed: track latest mutation LSN, require a covering checkpoint before unlink. A stale image retains the conservative original pin. R5 d1 test. | Wire checkpoint completions and per-core trackers into the reactor. |
| R5-W1 | Component fixed: terminal write errors return `AckOutcome::Indeterminate`, fence the log/core and reject late CQEs without publishing durability. WAL r5_w1 test. Arena reads reject fenced cores. | Recovery resolves possibly persisted outcomes; no exactly-once client protocol supplied. |
| R5-I1 | Format/component fixed: extent image v3 has a CRC-protected full incarnation, epoch and tombstone flag; validation requires expected identity/epoch and rejects tombstones. R5 i1 and lifecycle tests. | Durable deletion publication, authoritative metadata recovery and offline v2 migration are not supplied. |
| R5-L1 | Fail-closed correction: corrupt current image or recovery with only one valid copy requests repair. No silent older-copy fallback without proven retained WAL. Lifecycle corruption test. | A repair/replay service with established retention floor is required to recover degraded copies. |
| R5-T1 | Component fixed: journal replays names, IDs, incarnations and deletion records. Lookup/register preserve an existing incarnation; duplicate create rejects. R5 t1_t2_t3 and v30 s5. | Identity catalog/WAL/backup integration remains open. |
| R5-T2 | Conditional protection implemented: hash-chained acknowledged head is checked against mandatory independent CAS `IdentityAnchor`; rollback fails. R5 t1_t2_t3. | Only test-memory anchor exists here. Deploy a separately durable rollback-resistant provider; restoring journal and anchor together defeats the premise. |
| R5-T3 | Component fixed: truncate/sync only bytes beyond the independently acknowledged prefix. Missing/corrupt acknowledged records reject. Torn/zero suffix and corrupted-prefix tests. | Anchor durability/availability and disaster recovery need qualification. |
| R5-F1 | Component fixed: label fields private; public untrusted constructor produces External only. Higher levels require content-bound signed provenance from the caller's configured trusted issuer. Compile-fail doctests. | Trusted ingress must pin the issuer; a caller-supplied arbitrary public key is not a trust policy. |
| R5-F2 | Component fixed: Ed25519 approval binds content, field, exact nonzero sources, tenant/session, nonce, expiry, tool and sink. Bounded nonce tracker rejects reuse. R5 frame/v30 s6 tests. | Keep tracker for the session lifetime; fresh session IDs on restart, or persist replay state. |
| R5-F3 | Component fixed: labels alone never authorize side effects. Opaque one-use frame permit binds session/tool/sink/arguments. Wrong action/content and reuse tests. | All actual tool adapters must consume this permit and freshly verify request capabilities. |
| R5-F4 | Component fixed: separate Ed25519 signer/verifier types; verifier has only the public key. Wrong-issuer/signature tests. | Issuer key provisioning, rotation and secret isolation remain external. |
| R5-F5 | Component fixed: frame admission checks reader membership before admitting content; R5 foreign-reader test. | End-to-end frame compiler and gateway service remain open. |
| R5-P1 | Component fixed: empty canonical prefix path matches bucket/directory descendants; R5 root-prefix tests. | Bucket/path policy still comes from trusted configuration. |
| R5-P2 | Component fixed: tenant operator scans produce `QueryScope::Tenant`; fleet backup fails closed. Executor already filters to session tenant. | Separately rooted fleet administration API is not implemented. |
| R5-P3 | Fail-closed correction: policy rejects `HydrateFrame` rather than authorizing a naked hash. | Authenticated frame-handle resolver and frame store are not implemented. |
| R5-P4 | Partial: tenant-bound destination rules require S3 expected owner; Linux `LocalExportRoot` uses directory-relative `openat2` BENEATH/NO_SYMLINKS/NO_MAGICLINKS, exclusive creation and no unsafe fallback. Traversal/symlink/foreign-tenant tests. | S3 request adapter with enforced ExpectedBucketOwner and complete backup service are absent; reference engine exports remain unsupported. Trusted root must not be attacker writable. |
| R5-Q1 | Component fixed: independent vector bound 4,096 and token bound 16,384; 256/4,096 accepted, 4,097 rejected. | Work/result memory budgets still apply in execution. |
| R5-Q2 | Component fixed: unknown functions reject at parsing and public AST validation. | No general-purpose expression evaluator claimed. |
| R5-Q3 | Component fixed: `$ref:0xID.32hex` preserves ID plus 128-bit MAC as the security resolver's canonical RefToken. Bare reference IDs reject. | Executor does not dereference vault tokens; trusted resolver must verify context/expiry on use. |
| R5-Q4 | Component fixed for query validation/destruction: iterative preflight rejects deep AST before recursive walks, Query Drop drains nodes iteratively. 50,000 nested nodes tested on 128 KiB thread stack. | Raw public nested nodes and derived Clone/Debug/PartialEq are trusted Rust APIs; validate before applying recursive utilities, and wrap untrusted nodes in Query. |
| R5-Q5 | Component fixed: IN/CONTAINS accept mixed case; R5 parser test. | Existing root/field names retain their documented grammar. |
| R5-Q6 | Component fixed in reference execution: opaque `ValidatedQuery` holds tenant/session/owner version; executor checks version before exclusive execution. | Version is the current owner operation sequence, not a distributed routing epoch. External catalogs need consistency protocol. |
| R5-X1 | Component fixed: probe sealed O_RDWR descriptor against mprotect upgrade and new writable mmap; negative unsealed probe. Rejected CVE number removed from active docs/code. | Kernel behavior probe is not a general security certification. |
| R5-X2 | Component fixed: OwnedFd/BorrowedFd and RAII mapping/budget guards; only borrowed agent fd escapes; HugeTLB arena fd stays private. | Real SCM_RIGHTS/session/export publication remains integration work. |
| R5-A1 | Component fixed: expected full incarnation checked inside validated snapshot; wrong-incarnation R5 test. | Caller obtains expected identity from trusted authoritative metadata. |
| R5-A2 | Corrected claim: CoreControl is engine-private heap state. Agents never map the canonical arena; sealed projections are separate. | No exported shared control page or cross-process snapshot protocol exists. |
| R5-A3 | Component fixed: WordsMut constructor is crate-private; public writes obtain it through guards. | Trusted low-level arena APIs still need owner discipline and unsafe-code review. |
| R5-A4 | Component fixed: root types re-export canonical slot/seqlock implementations; persistence and public callers use the same types. R5 type/size guard. | Legacy-audit intentionally retains old vulnerable implementation behind an opt-in feature. |
| R5-O1 | Component fixed: cumulative consumed index makes duplicate/reordered acknowledgements idempotent; future acknowledgements reject. Separate PriorityMesh control/data capacity. | Reactor must prioritize control draining and wire cumulative indices to peer messages. |
| R5-V1 | Component fixed: conflicting duplicate ballot returns Conflict; identical replay returns Duplicate. v30 vote test. | Integrated transaction service remains open. |
| R5-S1 | Component fixed: equal-LSN mutation returns DuplicateLsn; internal checkpoint header stamping is separate. R5 slot test. | Replay must apply its explicit skip/already-applied policy. |
| R5-L2 | Component fixed: object/ticket/operation/state checks precede decrement; stale CQEs count and return. | Additional fault-schedule campaigns remain open. |
| R5-C1 | Intentional strict revocation: KeyEpoch equality remains the default. Rotation invalidates outstanding tokens and requires reauthentication. | Optional overlapping verification keyring is not implemented or claimed. |
| R5-C2 | Partial: mandatory signed Session/Audience caveats reject transfer between contexts. R5 c2 tests. | Session establishment, channel binding, command-level replay/idempotency and remote authenticated transport remain open. Macaroons remain bearer capabilities within their valid scope. |
| R5-C3 | Component fixed: reference MAC binds tenant/session/actor/expiry and location/generation/digest; missing/forged/expired share Denied. SHA-256-neutral field name `digest`. | Hash-map timing is not constant-time and no external vault service is supplied. |
| R5-B1 | Open production gate, explicitly retained. | No O_DIRECT/RWF qualification, fsync/power-cut harness, integrated recovery or physical NVMe power-loss testing was performed. |
| R5-G1 | Improved: original defect tests preserved as evidence, corrected guards added, concurrent mesh test has progress deadlines and external repeated runner uses subprocess timeouts. | Long-running fuzz/property/model-checking campaigns remain open. |
| R5-G2 | Attribution corrected: the independent reviewer reports its own Miri/aarch64 work on the older snapshot. | We did not execute Miri, loom or aarch64 for this revision; those results are not carried forward as local evidence. |

## Capacity and performance corrections

The canonical slot header and image header are 128 bytes each. Dense dirty arrays are 32 bytes per tenant slot (30.52 MiB for a million), allocated across owner partitions rather than a full global million-slot array per core. The actual route entry is 16 bytes before metadata/allocator overhead; a proposed 32-byte route is a design budget, not current structure size. The measured x86_64 Label object is 160 bytes plus up to 64 bytes of allocated reader payload and allocator overhead. Label size includes content digest, issuer, tenant/session scope, expiry and a heap reader set; measure the new type rather than reusing a 40-byte old-label estimate. Signature/provenance objects and retained journal images also consume memory.

One million registered tenants on 16–20 GiB remains an unqualified capacity objective. No SQL speedup, universal 200 ns request latency, production ACID guarantee or one-million-concurrent-user result is established by these component tests. Updated local smoke measurements are in [ECOSYSTEM_BENCHMARK.md](ECOSYSTEM_BENCHMARK.md).

## Primary references

Ed25519 verification uses upstream [ed25519-dalek](https://docs.rs/ed25519-dalek/2.1.1/ed25519_dalek/); file locking follows [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock). The relevant sealing fix appears in the [Linux 6.1.149 changelog](https://www.kernel.org/pub/linux/kernel/v6.x/ChangeLog-6.1.149), commit `8ec396d05d1b`; we make no claim that one probe certifies a kernel.

See [VERIFICATION.md](VERIFICATION.md) for commands actually executed and remaining qualification gaps.
