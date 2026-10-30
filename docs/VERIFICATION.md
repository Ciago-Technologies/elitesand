# Verification of the reviewed workspace

Verified on 30 October 2026 using rustc 1.99.1 on Linux x86_64.

| Command                                            | Observed result                                                                           |
| -------------------------------------------------- | ----------------------------------------------------------------------------------------- |
| `cargo test --release`                             | 56 passed, 0 failed, including 24 corrected v3 regressions                                |
| `cargo test --release --offline --all-features`    | 84 passed, 0 failed; includes optional historical defect reproductions                    |
| `cargo fmt --all -- --check`                       | Passed                                                                                    |
| `python3 scripts/validate_workspace.py`            | Passed: DAG, dependency boundaries, opt-in legacy API, embedded source and links          |
| `cargo clippy --workspace --all-targets --offline` | Completed successfully with P0 and vendored-source warnings                               |
| Vendored source comparison                         | All 10 packages byte-identical to downloaded upstream archives; SHA256 inventory retained |

The strict Clippy `-D warnings` gate was attempted and did not pass. Existing lint debt remains; ordinary Clippy completion does not imply a clean strict gate. No Miri, loom, cross-target, long-running fuzz campaign, real power-cut test, integrated ACID certification or performance SLO qualification was run.

The ZIP's original adversarial assertions passed when defects existed. Only `v30_regressions` asserts the corrected review contracts. See [REVIEW_REMEDIATION.md](REVIEW_REMEDIATION.md) for the remaining production gates.

VERDICT: PASS for the listed component tests and structural checks; production sign-off remains open.
