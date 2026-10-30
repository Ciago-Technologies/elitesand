# Supplied v3.0 review provenance

`original_lib.rs.txt` is the archive's `src/lib.rs.orig` (before its two compile corrections). `original_adversarial.rs.txt` is the archive's complete defect-reproduction suite. They are evidence text, not enabled tests or production APIs.

The corrected kernel is integrated into `coagomem-core/src/lib.rs` with typed parsing delegated to `coagomem-ql`. Security, policy and dirty-list modules replace the corresponding reference helpers. Corrected regressions are in `tests/v30_regressions.rs`. See `docs/REVIEW_REMEDIATION.md` for the full disposition, including open production gates.
