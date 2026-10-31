# ciagomem-r7-fix-patch

Fixes for R7-1 (unpinned logged LSN at the N7 checkpoint barrier), R7-2 (normal cold
load contract: `RecoveryBounds::clean`) and R7-3 (non-monotonic replay-tracker clock)
against `ciagomem-r6-fixed-review.zip`.

Apply from `ciagomem-slae/`:

    patch -p1 < patch/r7-fixes.diff
    cargo test --release --offline --workspace --all-features   # 144 passed observed
    cargo test --release --offline -p coagomem-core --test r7_regressions

`patched-files/` holds full copies of every changed file.

Breaking change: `TruncationPins::on_dirty` no longer returns `CheckpointInProgress`;
call `check_admission(t)` before assigning an LSN. A record logged during a checkpoint
keeps the older covering pin and makes that checkpoint stale.

Reviewer probes (`probes/r7_review.rs`): add to `tests/` plus a `[[test]] r7_review`
entry in `coagomem-core/Cargo.toml`. On the unpatched upload all 7 pass (4 defects
present). On the patched tree `defect_r7_1` and `defect_r7_3` fail (fixed); the
`defect_r7_2*` probes intentionally still pass because they use the global interval,
which remains fail-closed.

`logs/` contains raw outputs: upload default/all-features, patched suites, Miri
(core lib, R6 integration subsets, R7 acceptance) and the aarch64 compile check.
clippy, rustfmt, loom/Shuttle, fuzzing, ARM execution and storage qualification
were not run.
