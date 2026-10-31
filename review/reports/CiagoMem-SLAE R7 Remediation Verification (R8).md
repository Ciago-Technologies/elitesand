# CiagoMem-SLAE R7 Remediation Verification (R8)

Input: `ciagomem-r7-fixed-review.zip` (current five-crate `ciagomem-slae/` workspace plus the R7 master specification, 644 lines). `New File/` and `review/` were treated as evidence only and nothing in the current source was overwritten. I read `docs/R7_REMEDIATION.md`, `docs/VERIFICATION.md` and `README_UPLOAD.md` before inspecting code. Line numbers refer to the uploaded source unless marked "patched".

## 1. Verdict

The supplied R7-1, R7-2 and R7-3 fixes are present unchanged or slightly hardened, and their behavior is confirmed by tests and Miri. The two boundary fixes the authors added beyond the supplied patch (R7-4 control binding, R7-5 engine entry checks, R7-6 recovered-durability publication) work for the tested cases but introduce or leave three new defects:

- R8-1 (Medium): the durability watermark that R7-4/R7-6 publish is shared without exclusivity. A second log bound to the same core, or a reopen whose recovered prefix is shorter than an earlier published watermark, makes unwritten records readable as `Durable`.
- R8-2 (Low): the side-effecting local export adapter still accepts expired and earlier-boot capabilities; R7-5's "every public token-taking operation" claim does not hold outside `Engine`.
- R8-3 (Low): R7-5 made `Engine` neither `Send` nor `Sync` (it was both before), which blocks moving or sharing an engine across core threads.

All three are fixed in the attached patch with acceptance tests. Production readiness and million-user capacity remain unverified; nothing here measures them.

## 2. What was executed

| Check | Upload (as received) | Patched tree |
| --- | --- | --- |
| `cargo test --release --offline --workspace` | 120 passed, 0 failed (matches claim) | 125 passed, 0 failed |
| `... --all-features` | 148 passed, 0 failed (matches claim) | 153 passed, 0 failed |
| `scripts/validate_workspace.py` | PASS | PASS |
| `scripts/check_mesh_progress.py` | 1,000 + 25 runs PASS | PASS |
| `SOURCE_SHA256.json` | 1,808 files checked; only the two `Cargo.toml` files I edited to register probes differ | n/a |
| Patch applied to a fresh extraction | n/a | `patch -p1` clean, 153 passed |
| Miri (nightly), `coagomem-core` `r7_regressions` + `r8_regressions` (export test skipped: `openat2` syscall unsupported by Miri) | n/a | 7 passed, no UB reported |
| Miri, `coagomem-wal` `r7_recovery` + `r8_recovery` | n/a | 3 passed, no UB reported |
| `cargo check --target aarch64-unknown-linux-gnu --workspace --all-targets --all-features` | n/a | exit 0 (compile only) |
| R7 probes (my previous harness) | `defect_r7_1`, `defect_r7_3` now FAIL (fixed); `defect_r7_2*` still pass by design (they use global crash bounds); guards pass | same |
| R8 probes (new, this review) | 3 of 3 PASS (defects present) | WAL probes now panic at the new guard (fixed); export probe no longer compiles against the changed API, and its acceptance test replaces it |

Not run: `cargo fmt` and `cargo clippy` (components not installed in this offline sandbox, so I cannot confirm the authors' claims that formatting passed and Clippy completed), the full Miri run on `coagomem-core` lib and engine (earlier full lib run took ~24 min; not repeated for R8), [loom](https://docs.rs/loom/latest/loom/) or [Shuttle](https://github.com/awslabs/shuttle) model checking, [cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html) campaigns, ARM execution, power-cut/NVMe qualification in the style of [ALICE](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai), and any benchmark. The upload's benchmark numbers (1,118 ns indexed query, etc.) are local smoke results and say nothing about capacity.

## 3. R7 fix verification

| ID | Claim | What I inspected | Result |
| --- | --- | --- | --- |
| R7-1 | Logged LSN never loses its pin | `dirty.rs` 40–131 is byte-identical to the supplied patch: `on_dirty` only rejects zero/regressing LSNs, `check_admission` gates before LSN assignment, stale completion clears the barrier and keeps the pin. R6 hot-tenant tests now call `check_admission` first. | Correct at component scope; Miri clean. No production caller exists (grep: only tests construct `TruncationPins`), so the "admit, append, pin" ordering is still a host obligation. |
| R7-2 | Clean cold loads independent of global WAL | `lifecycle.rs` 108–125 `RecoveryBounds::clean`; `covers` accepts exactly `checkpoint_lsn`; `admit_load` 488–493 rejects the `u64::MAX` case (`checked_add` yields start 0 → `InvalidRecovery`). Randomized lifecycle test now tracks `Checkpointed` LSNs. The A/B test was changed so a corrupt current copy on a normal load requests repair instead of falling back to the older copy, which matches the "no silent fallback" contract. | Correct. Note: `Event::Checkpointed` carries no image epoch, so the host must record the epoch it was running under at flush time to call `clean(…, image_epoch, …)`. |
| R7-3 | Timestamp regression cannot revive replay | `security.rs` 228–231 `observe`; 306–363 every expiry/provenance/approval/nonce check now uses the high-water clock. | Correct. As documented, a single bogus large trusted timestamp fails closed (all sessions appear expired until restart). |
| R7-4 | Unrelated control arrays rejected | `gclog.rs` 308–368 binds `Arc<[CoreControl]>` + core ID; `on_cqe` 704–717 no longer takes a control reference and adopts a foreign fence; `validate_arena` 370–376 uses `Arc::ptr_eq`. | Partially closed, as the ledger says: `validate_arena` is opt-in and unused outside tests, `SlotArena::new` still accepts any array. Binding is not exclusive (R8-1). |
| R7-5 | Engine rejects expired/foreign-boot tokens | `runtime.rs` 233–241 `authorize`; I enumerated all 13 `pub fn` on `Engine` that take `&VerifiedMacaroon`: every one calls `authorize` (some twice). | Correct inside `Engine`. Regressed `Send`/`Sync` (R8-3). Outside `Engine`, `LocalExportRoot::create` (R8-2) and `join::equi_join` take a capability without time/boot checks; `equi_join` only processes caller-held snapshots, so I record it as a documentation overclaim, not a defect. |
| R7-6 | Recovered durability published after superblock | `gclog.rs` 377–383 publishes `rec.next_lsn - 1`; the crash-cycle test's manual publish was removed and still passes. | Correct for fresh controls. Because `publish_durable` is `fetch_max` (`slot.rs` 252–257, see [Rust atomics](https://doc.rust-lang.org/std/sync/atomic/index.html)), it cannot lower a stale watermark on a reused control block (R8-1b). |

Earlier guarantees re-checked by running the full R5/R6/v3.0 suites (all pass) and by reading the changed callers: N1 per-core fence and mutexed publication unchanged; N2/N3/N5 code untouched; the `openat2` resolve flags in `destination.rs` are still `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS` (0x08|0x04|0x02, per the [openat2 man page](https://man7.org/linux/man-pages/man2/openat2.2.html)). Layout is unchanged: `CoreControl` 256 B (still 256 B after the R8-1 fix), `SlotHeader` 128 B, dirty arrays 33 B/slot, replay tracker +8 B.

## 4. Findings

### R8-1 Medium — durability watermark binding is not exclusive and can be stale

Classification: confirmed defect (component level); trigger requires host misconfiguration or a recovery result shorter than a previously published watermark in the same process.

Location: `coagomem-wal/src/gclog.rs` 308–316 (`open` checks only index and fence), 377–383 (`superblock_durable` publishes), `coagomem-core/src/slot.rs` 252–257 (`publish_durable` = `fetch_max`).

Mechanism: the per-core watermark is the only thing `SlotArena::read(.., Visibility::Durable, ..)` consults. R7-4 made the log own a reference to it but did not make the log the only publisher.

- (a) Two `GroupCommitLog`s opened on the same `controls` and `core_id` both publish. Log A's watermark makes log B's never-written LSNs pass `Durable` reads.
- (b) A same-process reopen (the pattern the crash-cycle test uses and R7-6 relies on) with `rec.next_lsn - 1` below the previously published watermark keeps the old value, so records appended after reopen are `Durable` before any CQE. Recovered < published can happen after media corruption or a device that lied about durability ([LWN on fsync error semantics](https://lwn.net/Articles/752063/)); in a real process restart the controls are fresh and (b) does not apply.

Consequence: data marked `Durable` may leave the node (agent handoff, external side effects) and then be lost on the next crash.

Reproduction (upload, both PASS): `coagomem-wal/tests/r8_review.rs` → `defect_r8_1_two_logs_share_one_core_watermark`, `defect_r8_1_reopen_with_shorter_recovery_keeps_stale_watermark`.

Smallest compatible fix (patched `slot.rs` 230–277, `gclog.rs` 305–333): add a `log_bound: AtomicBool` to `CoreControl` (fits in existing padding; size stays 256 B) with `bind_log()` (CAS) / `unbind_log()`; `GroupCommitLog::open` asserts the claim and that `controls[core].durable() <= rec.next_lsn - 1`, releasing the claim before panicking; `impl Drop for GroupCommitLog` releases it. Assertions match the existing `open` style; no format or dependency change. Hosts that legitimately recover a shorter prefix must allocate a fresh control array and fresh arenas.

Acceptance: `coagomem-wal/tests/r8_recovery.rs` → `r8_1_second_log_on_same_core_is_rejected_until_first_is_dropped`, `r8_1_reopen_rejects_watermark_beyond_recovered_end_and_accepts_exact_prefix`; `tests/r8_regressions.rs` → `r8_1_core_control_layout_and_single_publisher_claim`. The full crash-cycle suite still passes.

Exit criteria beyond the patch: a node constructor that allocates controls, opens exactly one log per core and calls `validate_arena` for every class before serving (still absent).

### R8-2 Low — local export adapter accepts expired and earlier-boot capabilities

Classification: confirmed gap; the same class as R7-5 but on a side-effecting path in `coagomem-core`.

Location: `coagomem-core/src/destination.rs` 17–34. `open(path, tenant)` stores no boot epoch and `create(session, relative)` has no time input; it checks tenant, `OP_OPERATOR | OP_READ` and path shape only.

Consequence: a host that caches a `VerifiedMacaroon` (the R7-5 ledger notes the benchmark previously did this) can create export files after expiry or after restart. The spec's "re-verify every request" rule mitigates this only if every host follows it.

Reproduction (upload, PASS): `tests/r8_review.rs` → `defect_r8_2_export_root_accepts_expired_and_prior_boot_capability` (token expiring at 100 ns, `create` succeeds when `MonotonicClock` reads ≥ 1,000 ns).

Smallest fix (patched `destination.rs` 11–42): `open(path, tenant, boot_epoch)`; `create(session, relative, now)` also rejects `session.boot_epoch() != self.boot_epoch || now >= session.expires_at()`. `now` must come from the trusted boot clock, mirroring `authorize_tool_call`. The only in-tree caller (`tests/r5_regressions.rs` p4) is updated.

Acceptance: `tests/r8_regressions.rs` → `r8_2_export_root_rejects_expired_and_prior_boot_capabilities` (no file created for expired or earlier-boot tokens; fresh token still creates).

### R8-3 Low — R7-5 made `Engine` neither `Send` nor `Sync`

Classification: confirmed API/composition regression (compile-time; fails safe, no memory-safety impact).

Location: `ciagomem-engine/src/runtime.rs` 93–94 (`clock: Arc<dyn Clock>`, `last_now: Cell<u64>`), 207, 222, 233–235.

Evidence: a `fn f<T: Send>()` / `fn f<T: Sync>()` assertion on `Engine<MemoryJournal>` compiles against the R7 reviewer's patched tree and fails on the upload with E0277 (`Cell<u64>` cannot be shared; `dyn Clock` cannot be sent/shared). `Cell` is never `Sync` ([Rustonomicon](https://doc.rust-lang.org/nomicon/send-and-sync.html)). An engine can no longer be built on one thread and moved to its core thread, or shared read-only for `&self` snapshot paths.

Smallest fix (patched `runtime.rs` 92–94, 205–242): `Arc<dyn Clock + Send + Sync>` and `AtomicU64` with `fetch_max`, giving the same high-water semantics. Existing callers (`MonotonicClock`, test clocks) coerce unchanged.

Acceptance: `ciagomem-engine/tests/r8_engine.rs` → `r8_3_engine_is_send_and_sync`; the existing `r7_5_engine_rejects_expired_and_foreign_boot_tokens_at_every_entry` still passes.

### R8-4 Info — `#[inline(always)]` moved off `SlotArena::core`

Location: `slot.rs` 485–489. Inserting `controls()` between the attribute and `core()` moved the hint to the cold accessor. No correctness impact and no measured performance impact; restored in the patch.

### R8-5 Info — documentation overclaims

- `R7_REMEDIATION.md` R7-5 and spec line 190: "every public token-taking operation" is true only for `Engine`; `LocalExportRoot::create` (fixed by R8-2) and `join::equi_join` are outside it.
- Spec lines 37, 164 and 166 describe WAL binding without saying it is non-exclusive and monotonic-only (R8-1).
- `VERIFICATION.md` reports fmt and Clippy runs I could not reproduce here; treat them as author-reported.

## 5. Tested behavior vs. missing production integration

Tested at component level: pin retention across checkpoints, exact clean-image loading, monotonic tracker clock, control-array identity checks, recovered-prefix publication, engine entry-time expiry/boot checks, and (patched) single-publisher control binding and export capability freshness.

Missing, so none of the above is end-to-end: node/reactor constructor (allocate controls, one log per core, register and validate every arena), the WAL replay/repair driver, the production identity anchor, durable per-tenant checkpoint metadata including image epoch, the transport authentication/peer-credential layer, the transaction service, and storage qualification. The 33 B/slot dirty overhead and the 256 B per-core control are the only memory figures supported by evidence; no RSS, tail-latency or user-count claim follows.

## 6. Patch contents (`ciagomem-r8-fix-patch.zip`)

- `patch/r8-fixes.diff` — 348 lines, apply from `ciagomem-slae/` with `patch -p1`; dry run and full apply verified on a fresh extraction of the upload.
- `patched-files/` — full copies of `coagomem-core/src/{slot.rs,destination.rs}`, `coagomem-core/Cargo.toml`, `coagomem-wal/src/gclog.rs`, `ciagomem-engine/src/runtime.rs`, `tests/{r5_regressions.rs,r8_regressions.rs}`, `coagomem-wal/tests/r8_recovery.rs`, `ciagomem-engine/tests/r8_engine.rs`.
- `probes/` — `coagomem-wal/tests/r8_review.rs`, `tests/r8_review.rs` (PASS while defects exist).
- `logs/` — upload default/all-features, R7 and R8 probe output, patched default/all-features, Miri subsets, aarch64 check.

API changes: `LocalExportRoot::open` gains `boot_epoch`, `create` gains `now`; `Engine::new` clock type tightens to `Arc<dyn Clock + Send + Sync>`; `GroupCommitLog::open` panics on a duplicate core binding or stale watermark; new `CoreControl::{bind_log, unbind_log}`. No dependency, persistent-format or size change.

## 7. Exact specification corrections

Line 37, replace "bind the WAL to its controls, publishing recovered durability after superblock confirmation." with:

> bind the WAL exclusively to one core's controls (a second binding or a control block whose watermark exceeds the recovered end is rejected at open), publishing recovered durability after superblock confirmation.

Line 164, replace "The WAL binds its controls at open and publishes the recovered end after superblock_durable confirms the reopened superblock, before Durable reads can succeed." with:

> The WAL claims its core's controls exclusively at open and publishes the recovered end after superblock_durable confirms the reopened superblock. Publication is monotonic (fetch_max), so open rejects a reused control block whose watermark exceeds the recovered end; recovering a shorter prefix requires fresh controls and arenas.

Line 166, replace "WAL open binds the array/core ID and CQE processing cannot substitute another control object." with:

> WAL open binds the array/core ID, claims the core's single publisher role until the log is dropped, and CQE processing cannot substitute another control object.

Line 190, replace "rejecting expired/foreign-boot verified tokens at each public token-taking entry;" with:

> rejecting expired/foreign-boot verified tokens at each public `Engine` entry, and `LocalExportRoot` is bound to the boot epoch and requires the trusted current time for each export;

and append: "The engine remains `Send + Sync`; its clock must be `Send + Sync`."

Line 618, replace the "Current disposition" cell with:

> R8 reviewer ran Miri on the R7 core acceptance tests, the R8 core layout test and the WAL R7/R8 recovery tests (no UB reported), plus an aarch64 compile check of the R8-patched workspace. Full-library Miri for R7/R8 changes, engine Miri, loom/Shuttle and ARM execution remain open.

## 8. Acceptance checklist for the next revision

1. `cargo test --release --offline --workspace --all-features` ≥ 153 passed with the R8 tests present.
2. Both R8 WAL probes panic at `open`; the export probe is replaced by the R8-2 acceptance test.
3. `Engine<MemoryJournal>: Send + Sync` compile assertion passes.
4. A node constructor test proves one log per core and `validate_arena` for every class before the first request (still open).
5. Miri on the full `coagomem-core` lib after the `CoreControl` change; loom or Shuttle model of `bind_log`/`publish_durable`/`fence` (open).
