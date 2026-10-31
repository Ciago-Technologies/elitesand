# R8 remediation — 31 October 2026

> Historical ledger. The [R9 ledger](R9_REMEDIATION.md) supersedes publisher access and pending-device-I/O teardown contracts.


Input: the latest **CiagoMem-SLAE R7 Remediation Verification (R8).md** and `ciagomem-r8-fix-patch.zip` under New File/. All three supplied runtime probes passed before changes: two reproduced false Durable visibility and one reproduced stale export authorization. Their originals, patch, full files and external logs are retained as text under [review/r8](../review/r8). The supplied patch applied cleanly; local formatting and one additional initialization-failure guard were then applied.

| Finding | Correction and evidence | Boundary |
| --- | --- | --- |
| R8-1 — duplicate/stale durability publishers | CoreControl atomically claims one log role. GroupCommitLog::open rejects a duplicate claim and a published watermark above the recovered end. A private RAII binding owns the claim, releasing it on log destruction or any failed initialization; zero next-LSN and exhausted epoch/sequence metadata reject rather than wrapping. Tests cover duplicate open, reopening after drop, shorter/exact recovery, unchanged 256-byte control layout, and failed initialization followed by a valid reopen. | Open uses fail-closed startup assertions, matching the component's existing API. A node must still allocate controls and register/validate every class. bind/unbind/publish are trusted low-level host operations, not protection against arbitrary Rust host code. Drain/cancel outstanding I/O before destroying a log, as required by the existing buffer-lifetime contract. |
| R8-2 — stale export authority | LocalExportRoot is configured with the current boot epoch. create requires trusted current time and checks token boot/expiry in addition to tenant/permissions/path enforcement. Acceptance verifies expired/old-boot attempts create no file while a fresh token still works. Existing traversal/symlink tests were updated and still pass. | The host must supply fresh trusted boot-clock time, not client input or a cached earlier timestamp. This is entry-time authorization; an already returned File is not revoked automatically at token expiry. |
| R8-3 — Send/Sync regression | Engine uses an Arc clock requiring Send + Sync and an AtomicU64 high-water timestamp. The compile-time Engine<MemoryJournal>: Send + Sync test passes; existing expiry/foreign-boot/backward-time tests still pass. | Trait support depends on the configured Journal also supporting Send/Sync. Mutable operations still require exclusive access; this does not implement concurrent reactor dispatch. |
| R8-4 — inline annotation | Restored inline(always) on SlotArena::core rather than the cold controls accessor. | No independent instruction-level speedup claim. |
| R8-5 — documentation scope | Master specification and README now distinguish Engine entry checks, local export checks and pure helpers over caller-held snapshots. WAL claims explicitly describe exclusive binding, monotonic publication and shorter-recovery rejection. | join::equi_join does not independently authenticate or expire a capability; invoke it only on snapshots obtained through an authorized entry. None of these components is a complete service. |

Recovering a shorter prefix requires fresh controls and fresh arenas; reusing a stale watermark is rejected. Allocating fresh memory is not permission to discard previously acknowledged data: an operator/recovery system must investigate that loss and repair or explicitly resolve it before serving. A production recovery/repair service and storage qualification remain absent.

## Cleanup and policy consolidation

There is one engineering policy record: [policy.json](../policy.json), updated in place. Six stage-specific policy JSON files were removed; runtime authorization source policy.rs remains required code.

Removed 11 obsolete ZIPs, three obsolete directories (the original extracted P0/audit trees and New File/Old), and three superseded review prompts. Before deleting the extracted trees, 23 non-generated source/evidence files were preserved as text under review/initial-source. Four historical reports moved to review/reports; previous patch/probe/log evidence remains under review. Active Cargo metadata proves dependencies resolve entirely inside the current workspace. The current input ZIP/report, current workspace/Git state, vendor licenses and latest outgoing review bundle are retained. Exact deletions and preserved-source hashes are in [cleanup-manifest.json](cleanup-manifest.json).

## Verification limits

Locally run results are in [VERIFICATION.md](VERIFICATION.md). The external reviewer ran selected Miri tests and an aarch64 compile check on its supplied patch; those results are attributed external evidence and do not certify our additional RAII initialization changes. Full current Miri/loom/Shuttle, ARM execution, long fuzz campaigns, power cuts, production anchor, durable checkpoint catalog, node/reactor startup and replay/repair services remain open. No million-user, RSS, SQL speedup or production sign-off claim is made.
