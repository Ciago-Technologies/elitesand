# Ciagomem-SLAE R6 Remediation Verification (R7)

Scope: `ciagomem-r6-fixed-review.zip`, covering the current five-crate workspace `ciagomem-slae/` and the 640-line master specification. Historical material under `New File/` and `review/` was treated as evidence, not source.

I checked fixes N1–N7 against their connected callers: lifecycle, WAL group commit, frame admission, the engine runtime and the tests.

Environment: Linux 6.1.155, x86_64, 2 vCPU, Rust 1.99.1 stable plus nightly Miri, and an aarch64 target for compile checks only. Labels used below: "Observed" means I executed it. "Inspected" means I read the source only.

## 1. Verdict

The R6 remediation is real work, and its stated test evidence reproduces exactly on the pristine upload.

- N2, N3 and N5 are correct and covered.
- N1 is correct at component scope.
- N4 and N6 are partially fixed.
- **N7 is incorrectly specified at its API boundary.** `TruncationPins::on_dirty` refuses an LSN during a checkpoint. A caller can only hold an LSN after the record is in the log, so the retry later fails with `LsnRegression`. The record is then never pinned, and WAL truncation may pass it (R7-1, data-loss hazard).

I found three confirmed defects (R7-1 to R7-3) and three integration gaps (R7-4 to R7-6). The patch fixes R7-1 to R7-3 with about 290 diff lines, no new crates and no format change. After the patch, all existing tests still pass (144 all-features, 116 default), plus the new acceptance tests.

Production status is unchanged. This is a component and reference workspace with no production anchor, WAL replay driver, reactor, repair service or storage qualification. Nothing here supports a capacity or million-user claim.

## 2. Evidence executed

| Check | Tree | Observed |
|---|---|---|
| `cargo test --release --offline --workspace` | Pristine upload | 112 passed, 0 failed (matches VERIFICATION.md) |
| `... --all-features` | Pristine upload | 140 passed, 0 failed (matches) |
| `python3 scripts/validate_workspace.py` | Upload and patched | `VERDICT: PASS` |
| `python3 scripts/check_mesh_progress.py` | Patched | 1,000 cached-tail and 25 concurrent-grid runs passed |
| Miri, `cargo +nightly miri test -p ciagomem-core --lib` (`-Zmiri-disable-isolation`) | Upload | 18 passed, no UB reported (1,433 s; includes the shared-mutex `CoreControl`, lifecycle randomized interleavings, mesh grid, seqlock) |
| Miri, R6 integration tests `n1_`, `n5_n6`, `n7_` | Patched | 4 passed, no UB reported |
| Miri, R7 acceptance `r7_1`, `r7_2` | Patched | 3 passed, no UB reported |
| `cargo check --target aarch64-unknown-linux-gnu --workspace --all-targets --all-features` | Patched | Exit 0 (compile only) |
| Reviewer probes `tests/r7_review.rs` | Upload | 4 `defect_*` tests pass (defects present); 3 `guard_*` tests pass |
| Same probes | Patched | `defect_r7_1` and `defect_r7_3` now fail (fixed). The `defect_r7_2*` probes still pass by design: they use the global interval, which remains fail-closed |
| Patched suites | Patched | 144 passed with all features (140 + 4 acceptance tests); 116 passed by default |
| Layout | Upload | `Label`=176, `FrameValue`=320, `CoreControl`=256, `SlotHeader`=128 bytes |

**Not run:**
- `cargo clippy` and `cargo fmt`: the components are not installed and cannot be fetched offline.
- loom or Shuttle model checking ([loom](https://docs.rs/loom/latest/loom/), [Shuttle](https://github.com/awslabs/shuttle)).
- Fuzzing ([cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html)).
- aarch64 execution.
- Power-cut, NVMe or O_DIRECT qualification ([pwritev2](https://man7.org/linux/man-pages/man2/pwritev2.2.html), [ALICE](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai)).
- Any benchmark. I do not restate their benchmark numbers as capacity evidence.

Miri runs default single-seed schedules; it finds UB on the exercised paths and is not a weak-memory proof ([Miri](https://github.com/rust-lang/miri/)).

On the "inconsistent probe totals" note in R6_REMEDIATION.md: my R6 harness had 8 probes, 7 core plus 1 WAL. The R6 report's "135 = 128 + 7" counted only the core probes. Their count of eight is correct.

## 3. Verification of N1–N7

| Finding | Claim | Classification | Basis |
|---|---|---|---|
| N1 per-arena fences | Shared `Arc<[CoreControl]>`, private fields, per-core fence checked before and after snapshot validation | **Correct at component scope** | `slot.rs` 218–267, 524–580 inspected. Guard test: two classes share the fence; the other core stays readable. Their compile-fail doctest covers field privacy. Miri clean. Wiring remains host-trusted (R7-4) |
| N2 derived-label laundering | `label_pllm_output` clears issuer and scope | **Correct and covered** | `security.rs` 399–408. Derived non-External labels fail the issuer check in `admit_to_frame` |
| N3 incarnation reuse | 128-bit OS entropy per create | **Correct and covered** | `identity.rs` 303. `random_id` reads `/dev/urandom`, has no fallback and rejects 0. A VM RNG-restore caveat is documented |
| N4 replay state | TTL cap, per-tenant quota, min-heap, boot epoch | **Partially fixed** | TTL, quota, boot binding and session-expiry propagation are correct. Eviction uses caller `now` and can run backwards (R7-3) |
| N5 epoch / absent vs corrupt | `ImageStatus`, epoch `<=` current, trusted epoch | **Correct and covered** | `envelope.rs` 254–317. Absent only for all-zero extents; Invalid leads to NeedsRepair with `-EUCLEAN` |
| N6 single-copy recovery | `RecoveryBounds` coverage, Replaying state, `finish_replay` | **Partially fixed** | Crash-recovery logic is correct. The same gate applies to normal cold loads with no documented tenant-scoped interval (R7-2). Recovered durability is not published (R7-6). The replay driver is absent (acknowledged) |
| N7 hot-tenant pin | `begin_checkpoint` barrier blocks `on_dirty` | **Incorrectly fixed at API boundary** | `dirty.rs` 45–47 rejects an already-logged LSN; 51–53 rejects its later retry (R7-1). Safe only if the host checks the lifecycle gate before assigning an LSN, and the tracker offers no such pre-check |

Earlier R5 guarantees re-checked for regressions:
- Mesh progress: observed.
- W1 fence and late CQEs: `gclog.rs` unchanged; `r5_wal` now uses two arenas; observed.
- Arena incarnation and core check inside the seqlock snapshot: inspected and Miri.
- Parser, `ValidatedQuery` and engine tenant filtering: unchanged, all tests pass.
- Macaroon mandatory caveats: BootEpoch is added as mandatory and nonzero; inspected.

I found no regression in these.

## 4. Findings

### R7-1 — High: logged LSN can be left unpinned (N7 barrier)

- **Location:** `ciagomem-core/src/dirty.rs` 45–47 (`if self.checkpointing[i] { return Err(CheckpointInProgress) }`), 51–53 (`LsnRegression`), 96–98 (stale completion leaves `checkpointing` set). Specification line 131.
- **Invariant:** every logged record not contained in a durable image must hold a truncation pin.
- **Reproduction:** `tests/r7_review.rs::defect_r7_1_logged_lsn_rejected_by_barrier_is_never_pinned` (observed).
  1. `on_dirty(0,5)`, then `begin_checkpoint(0)`, which captures 5.
  2. The tenant-0 record is appended at LSN 6. `on_dirty(0,6)` returns `CheckpointInProgress`.
  3. Tenant 1 logs LSN 7. `on_checkpointed(0,5)` succeeds.
  4. The retry `on_dirty(0,6)` returns `LsnRegression`.
  5. `min_needed_lsn` returns 7, so LSN 6 is truncatable but is in no image.
- **Impact:** after truncation and a crash, an acknowledged mutation is lost. The trigger is the natural "append, then register" order, because `on_dirty` requires the LSN.
- **Smallest fix (patched):**
  - Add `check_admission(t)`, which hosts call before assigning an LSN.
  - `on_dirty` never refuses a valid logged LSN.
  - A stale completion releases the barrier and keeps the older covering pin.
  - Same O(1) arrays and no layout change. Two existing R6 assertions move from `on_dirty` to `check_admission`.
- **Acceptance:** `tests/r7_regressions.rs::r7_1_logged_lsn_during_checkpoint_stays_pinned`. LSN 6 stays pinned (`min_needed_lsn == 5`); a later covering checkpoint unpins. Existing N7 tests (989 hot-tenant cycles, lifecycle defer/failure) still pass.

### R7-2 — Medium: normal cold loads inherit crash-recovery coverage

- **Location:** `lifecycle.rs` 108–113 (`covers`), 650 (applies to `Probe::Current` loads), 677–690 (replay through `wal_end_lsn`). Specification line 160 defines the bounds only as "trusted recovered WAL interval".
- **Reproduction** (observed):
  - `defect_r7_2_clean_cold_load_after_wal_truncation_needs_repair`: a clean evicted tenant has checkpoint 5. After its pin was released the log was truncated (start 100, end 200). The load emits `NeedsRepair`.
  - `defect_r7_2b_...`: with the log retained, the same clean tenant gets `ReplayRequired { from 6, through 200 }`. The route only publishes after a scan to the global end plus a no-op write at LSN 200.
  - The in-tree randomized lifecycle test always passes `wal_start_lsn: 1`, which hides this.
- **Impact:** under the documented semantics, normal eviction plus truncation makes clean tenants unavailable. When the log is retained, every cold load pays a global suffix scan.
- **Smallest fix (patched, no struct change):** add `RecoveryBounds::clean(current_epoch, image_epoch, checkpoint_lsn)`, the empty tenant-scoped interval `[ckpt+1, ckpt]`. The existing `covers` check then loads exactly that image, and an older or newer image fails closed. Document that normal loads use tenant-scoped bounds from durable metadata, and crash recovery uses the recovered global interval.
- **Acceptance:** `r7_2_clean_cold_load_after_truncation_loads_without_replay` (loads at 5; stale image with metadata 6 requests repair) and `r7_2_dirty_tenant_with_truncated_suffix_still_needs_repair`.

### R7-3 — Medium: replay window reopens on non-monotonic request time

- **Location:** `security.rs` 225–255 (`NonceTracker::consume` evicts with caller `now`). Specification line 210 says approvals "retain replay protection until their actual expiry".
- **Reproduction:** `defect_r7_3_non_monotonic_now_reopens_replay_window` (observed).
  1. Nonce 1 is admitted at `now=10` (expires 500).
  2. Nonce 2 is admitted at `now=600`, which evicts nonce 1.
  3. Nonce 1 is replayed at `now=20` and accepted.
- **Trigger:** two request paths that read the clock before acquiring the tracker, or any host clock regression. The tracker is `&mut`, so it does not race itself, but the timestamps it receives can be out of order.
- **Smallest fix (patched):** keep a high-water `clock` in the tracker and use `max(now, clock)` for TTL, expiry and eviction. This fails closed, and costs 8 bytes.
- **Acceptance:** `r7_3_non_monotonic_now_cannot_reopen_replay` (replay is denied). Existing N4 tests pass.

### R7-4 — Low (integration): control-array identity is host-trusted

- **Location:** `slot.rs` 380–400 (constructors accept any `Arc<[CoreControl]>`); `gclog.rs` 754 (`core.publish_durable` on whatever `&CoreControl` the caller passes).
- **Reproduction:** `guard_r7_arena_accepts_unrelated_control_arrays_silently` (observed). Two arenas built from different arrays are accepted, and a fence on one is invisible to the other.
- **Fix (not patched; API addition):**
  - Add `SlotArena::controls(&self) -> &Arc<[CoreControl]>`.
  - Have the node constructor assert `Arc::ptr_eq` across all classes.
  - Pass the same `Arc` to the log object so it no longer takes an arbitrary reference.
- **Acceptance:** building a node with a mismatched array fails, and a log fence rejects reads on every class.

### R7-5 — Low (integration, inspected): engine does not re-check capability expiry or boot

- **Location:** `ciagomem-engine/src/runtime.rs` 187–198 (`allowed`), plus `put`, `snapshot`, `execute`, `delete` at 235–457. No `now` or engine-owned boot epoch is consulted; `VerifiedMacaroon::expires_at()` is unused.
- **Impact:** a host that keeps a verified capability for a long-lived connection keeps reading and writing after its `ExpiresAt`. `VerifiedMacaroon` is neither Clone nor serializable, so reuse across a restart is not possible.
- **Fix:** the engine holds its `SessionEpoch` and a clock and rejects `token.boot_epoch() != epoch || now >= token.expires_at()`. Alternatively, require per-request verification and state it.
- **Acceptance:** an engine call with an expired token returns `Permission`.

### R7-6 — Low (integration, inspected): recovered durability is not published

- **Location:** `gclog.rs` 304 (`open`) does not publish the recovered end. Only the test at line 1110 calls `publish_durable(rec.next_lsn - 1)`.
- **Impact:** after restart, Durable-visibility reads of replayed tenants return `NotYetDurable` until the next commit. This fails closed, so it is an availability issue only.
- **Fix:** take the shared controls in `open` and publish `next_lsn - 1` once the new superblock is durable, or specify this as a host step.
- **Acceptance:** after recovery, a Durable read of a replayed tenant succeeds without a new commit.

## 5. Memory arithmetic (layout only, not RSS)

- `Label` grew 160 → 176 bytes (boot epoch in scope); `FrameValue` grew 288 → 320.
- `CoreControl` is 256 bytes, so 14 shared cores take 3,584 bytes.
- Dirty arrays: 4+4+8+8+8+1 = 33 bytes per slot, or 31.47 MiB per million slots. This matches the ledger.
- Replay tracker (my estimate from type sizes): about 48 bytes per `HashSet` key plus 56 bytes per heap entry plus hash control and load-factor slack, roughly 110–130 bytes per live approval. Plus about 40 bytes per active tenant in `counts`. Example: 1,000 tenants × 64 nonces is about 7–8 MiB worst case. Hosts must charge `per_tenant × max_tenants`.
- R7-3 adds 8 bytes per tracker. R7-1 and R7-2 add no memory.

## 6. Patch

`ciagomem-r7-fix-patch.zip` contains:

- `patch/r7-fixes.diff`, which applies with `patch -p1` from `ciagomem-slae/`. It was checked by dry-run against the pristine upload.
- Full patched copies of `dirty.rs`, `lifecycle.rs`, `security.rs`, `ciagomem-core/Cargo.toml`, `tests/r6_regressions.rs` and the new `tests/r7_regressions.rs`.
- `probes/r7_review.rs`, the reviewer defect and guard probes.
- `logs/`, the raw outputs of the runs above.

The breaking change is small: callers that relied on `on_dirty` returning `CheckpointInProgress` must call `check_admission` before assigning an LSN.

## 7. Exact specification corrections

| Line | Current text (quoted) | Replacement |
|---|---|---|
| 131 | "These O(1) operations avoid a permanently stale hot-tenant pin at the cost of a checkpoint pause; an owner coordinator must call them before applying writes." | "These O(1) operations avoid a permanently stale hot-tenant pin at the cost of a checkpoint pause. The owner coordinator must call `check_admission` before assigning an LSN or appending a WAL record. `on_dirty` never rejects an already-logged LSN; a record logged during a checkpoint makes that checkpoint stale and retains the older covering pin." |
| 160 | "a caller-supplied interval must be established by recovery rather than guessed." | "a caller-supplied interval must be established by recovery rather than guessed. Normal cold loads use the tenant-scoped empty interval `RecoveryBounds::clean(current_epoch, image_epoch, checkpoint_lsn)` from durable metadata; crash recovery uses the recovered global interval. After recovery, publish the recovered end LSN to the shared core controls before serving Durable reads." |
| 210 | "accepted approvals retain replay protection until their actual expiry." | "accepted approvals retain replay protection until their actual expiry, measured on the tracker's monotonic high-water clock; a request timestamp earlier than that clock is evaluated at the clock." |
| 614 | "Not run locally for the R6 fixes; the independent reviewer ran Miri subsets and an aarch64 compile check on the preceding R5 source; loom/Shuttle remain open" | "Not run by the authors for the R6 fixes. An independent R7 review ran Miri on the full ciagomem-core library suite and the R6 n1/n5_n6/n7 integration tests (no UB reported), plus an aarch64 compile check, on the R6 source; loom/Shuttle, clippy and rustfmt gates remain open." |

## 8. Acceptance checklist

- [ ] Apply the patch: `r7_regressions` (4 tests) pass; `r7_review` `defect_r7_1` and `defect_r7_3` fail.
- [ ] `rg "return Err\(DirtyError::CheckpointInProgress\)" ciagomem-core/src/dirty.rs` matches only in `check_admission` and `begin_checkpoint`.
- [ ] Every normal cold-load caller uses `RecoveryBounds::clean`; a crash-recovery test with truncation still requests repair.
- [ ] Node constructor asserts one shared control array across all classes and the log (R7-4).
- [ ] Engine rejects expired or foreign-boot tokens (R7-5).
- [ ] Recovered end LSN is published before Durable reads (R7-6).
- [ ] loom or Shuttle on CoreControl publish/fence and the mesh; full Miri on integration tests; aarch64 execution.
- [ ] clippy `-D warnings` and `rustfmt --check` in an environment with the components.
- [ ] Storage power-cut qualification ([LWN fsyncgate](https://lwn.net/Articles/752063/)) and a production anchor and repair drill (still open).
