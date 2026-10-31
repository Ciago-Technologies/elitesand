# ciagomem-r8-fix-patch

Fixes against `ciagomem-r7-fixed-review.zip`:

- R8-1 (Medium): exclusive per-core WAL binding (`CoreControl::bind_log`/`unbind_log`,
  `Drop for GroupCommitLog`) and rejection of a reused control block whose durable
  watermark exceeds the recovered end.
- R8-2 (Low): `LocalExportRoot` bound to the boot epoch; `create` requires trusted `now`
  and rejects expired capabilities.
- R8-3 (Low): `Engine` is `Send + Sync` again (`Arc<dyn Clock + Send + Sync>`, `AtomicU64`).
- R8-4 (Info): `#[inline(always)]` restored on `SlotArena::core`.

Apply from `ciagomem-slae/`:

    patch -p1 < patch/r8-fixes.diff
    cargo test --release --offline --workspace                 # 125 passed observed
    cargo test --release --offline --workspace --all-features  # 153 passed observed

`patched-files/` holds full copies of every changed or added file.

API changes: `LocalExportRoot::open(path, tenant, boot_epoch)`,
`LocalExportRoot::create(session, relative, now)`; `Engine::new` clock argument is
`Arc<dyn Clock + Send + Sync>`; `GroupCommitLog::open` panics on a duplicate core binding
or a stale watermark. No dependency, persistent-format or struct-size change
(`CoreControl` stays 256 B).

Probes (`probes/`, pass while defects exist on the upload): copy
`coagomem-wal/tests/r8_review.rs` (auto-discovered) and `tests/r8_review.rs` plus

    [[test]]
    name = "r8_review"
    path = "../tests/r8_review.rs"

in `coagomem-core/Cargo.toml`. On the patched tree the WAL probes panic at `open`
(fixed); the export probe no longer compiles against the new signature and is replaced
by `r8_2_export_root_rejects_expired_and_prior_boot_capabilities`.

Not run: rustfmt, Clippy (not installed offline), full-library Miri, engine Miri,
loom/Shuttle, fuzzing, ARM execution, power-cut/NVMe qualification, benchmarks.
