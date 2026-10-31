# ciagomem-r6-verification-harness

Reviewer probes for the R5-fixed CiagoMem-SLAE workspace.

Install into `ciagomem-slae/`:
1. Copy `tests/r6_review.rs` to `tests/`, and `coagomem-wal/tests/r6_wal.rs` to `coagomem-wal/tests/`.
2. Append to `coagomem-core/Cargo.toml`:
   [[test]]
   name = "r6_review"
   path = "../tests/r6_review.rs"
3. Run:
   cargo test --release --offline -p coagomem-core --test r6_review
   cargo test --release --offline -p coagomem-wal --test r6_wal

`defect_*` tests PASS while the defect exists (R6-N1..N5); invert them after fixing.
`guard_*` tests pass when claimed protections hold (and document N7 liveness).
`logs/` holds the raw outputs from the review run (all-features suite, validator,
mesh runner, Miri subsets, aarch64 check).
