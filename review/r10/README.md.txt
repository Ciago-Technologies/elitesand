# ciagomem-r10-fix-patch

Fixes for R10-1..R10-4 against `ciagomem-r9-fixed-review.zip`. See the report for details.

## Apply
    cd ciagomem-slae
    patch -p1 < ../patch/r10-fixes.diff
    cargo test --release --offline --workspace                 # expect 151 passed
    cargo test --release --offline --workspace --all-features  # expect 179 passed

`patched-files/` holds full copies of the 12 changed files.

## Probes (reproduce on the unpatched upload)
Copy `probes/core/*.rs` to `ciagomem-slae/tests/` and add to `ciagomem-core/Cargo.toml`:

    [[test]]
    name = "r10_review"
    path = "../tests/r10_review.rs"

    [[test]]
    name = "r10_safe_review"
    path = "../tests/r10_safe_review.rs"

    [[test]]
    name = "r10_uring"
    path = "../tests/r10_uring.rs"

Copy `probes/wal/*.rs` to `ciagomem-slae/ciagomem-wal/tests/`.

- r10_review: R10-1 checkpoint unwind continuation (passes = defect present)
- r10_safe_review (`#![forbid(unsafe_code)]`): R10-2 SimIo heap corruption (aborts natively; Miri reports UB), `--ignored` SIGSEGV variant, R10-4 post_wake forged LogWrite token on a real ring
- wal r10_review: R10-3 fenced log never quiescent; R10-4 consequence (deliberate unsafe-contract violation)
- r10_uring (core and wal): positive evidence that R9 retention holds on a real io_uring ring

## Logs
r10_def/r10_all: upload 141/169. r10p_def/r10p_all: patched 151/179. r10_apply: fresh apply 179.
r10_miri_probe_*: Miri on probes against the upload. r10_miri_core/r10_miri_wal: patched acceptance subsets.
r10_miri_corelib: patched full core lib, no leak suppression, 18 passed. r10_arm: aarch64 check, exit 0.
