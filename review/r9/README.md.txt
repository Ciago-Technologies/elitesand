# ciagomem-r9-fix-patch

Fixes for R9-1 (High: safe drop with in-flight device I/O) and R9-2 (Low: advisory publisher exclusivity) against `ciagomem-r8-fixed-review.zip`.

## Apply
    cd ciagomem-slae
    patch -p1 < ../patch/r9-fixes.diff
    cargo test --release --offline --workspace                 # expect 134 passed
    cargo test --release --offline --workspace --all-features  # expect 162 passed

`patched-files/` holds full copies of all nine changed files if you prefer to copy them in.

## Probes (reproduce the defects on the unpatched upload)
- `probes/core/r9_review.rs` -> `ciagomem-slae/tests/r9_review.rs`, and add to `coagomem-core/Cargo.toml`:

      [[test]]
      name = "r9_review"
      path = "../tests/r9_review.rs"

- `probes/wal/r9_review.rs` -> `ciagomem-slae/coagomem-wal/tests/r9_review.rs`.

On the upload, the lifecycle probe crashes with SIGSEGV in release mode, and Miri reports use-after-free for both probes. After the patch, the publisher-bypass probe no longer compiles because those methods are crate-private.

## Logs
- r9_def / r9_all: upload, 126 / 154 passed
- r9p_def / r9p_all: patched, 134 / 162 passed; r9_apply: fresh-extract apply, 162 passed
- r9_miri_probe_{core,wal}: Miri on the probes against the upload (UB reported)
- r9_miri_wal: WAL r7/r8/r9 recovery under Miri, 7 passed
- r9_miri_core, r9_miri_core_r8: core r9 (3) and r8 layout (1) under Miri, passed
- r9_miri_corelib: full coagomem-core lib under Miri, no leak suppression, 18 passed
- r9_arm: aarch64 cargo check, exit 0 (compile only)
