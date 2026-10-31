# Verification of the R9 remediation revision

Executed locally on 31 October 2026 with rustc 1.99.1 stable on Linux x86_64. These are component/reference checks, not production qualification.

| Check | Observed result |
| --- | --- |
| Original probes in isolated child processes | Lifecycle probe reproduced SIGSEGV (core dumps disabled); two WAL probes reproduced the reported behavior |
| Supplied patch | Applied cleanly and passed 134 default / 162 all-feature tests before local hardening |
| Additional regressions before correction | Release/resubmit and lifecycle/WAL submission-unwind tests failed; captured failures guided the fixes |
| `cargo test --release --offline --workspace` | **141 passed, 0 failed** |
| `cargo test --release --offline --workspace --all-features` | **169 passed, 0 failed**, including optional historical defect assertions |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --offline` | Completed with warnings; no warning-free production gate claimed |
| `python3 scripts/validate_workspace.py` | Passed: dependency/legacy boundaries, embedded source, local links, R5–R9 dispositions, private publication APIs, one policy and preserved cleanup evidence |
| `python3 scripts/check_mesh_progress.py` | 1,000 cached-tail and 25 concurrent-grid runs passed with subprocess timeouts |
| Extracted review ZIP | **169 all-feature tests passed** fully offline with an empty Cargo home and separate target directory; extracted workspace validator passed |
| Vendor comparison | All 43 vendored source trees match original crate archives and their recorded SHA256 inventory |
| Benchmark | Not rerun for this I/O teardown change. [Earlier R8 smoke data](ECOSYSTEM_BENCHMARK.md) remains historical evidence, not a new measurement or capacity claim |

R9 has eleven runtime acceptance tests across core/WAL, covering late read/write after drop, normal completion, explicit device release, claim privacy/exclusivity, release followed by new submission, backend unwind after queuing, and rejected-submission rollback. Four new compile-fail doctests guard direct publisher access and unsafe completion boundaries. The existing R8 Engine Send + Sync and earlier recovery/security/query suites still pass.

The leak/retention path is deliberate for abandoned requests. Normal native tests do not prove leak-freedom or physical DMA correctness. Correct ordinary teardown requires draining/cancelling and reaping before owners are dropped. Unsafe release acknowledgements and CQE handlers require actual device-lifetime proof; source comments do not replace that proof.

Only stable Rust is installed locally. The reviewer reports Miri on its supplied patch (intentional-retention tests allow leaks; the full core library run did not) and an aarch64 compile check. Its preserved logs do not verify our additional submission/unsafe-boundary hardening. Current local Miri, engine Miri, loom/Shuttle, ARM execution, fuzz campaigns, real io_uring teardown, hardware power cuts and service-level durability were not checked.

Cleanup removed the superseded R8 input/output ZIPs and archived the R8 report; no additional obsolete directories were found. One policy.json and one current review prompt remain. A node/reactor, cancel-and-reap shutdown, durable engine Journal/replay/repair, production identity anchor and storage qualification are still open; no production or million-user claim is made.

See [R9_REMEDIATION.md](R9_REMEDIATION.md) for current contracts and limitations.

Project naming was subsequently normalized to Ciagomem and CiaQL, including archived review text and paths. Historical measurements remain measurements of the recorded revisions. Original review hashes and the pre-normalization Git revision are recorded in [naming-migration.json](naming-migration.json); current preserved-source hashes are in [cleanup-manifest.json](cleanup-manifest.json). Vendored sources were unchanged.

After the naming migration: 141 default and 169 all-feature release tests passed offline; formatting, the runnable quickstart, 1,025 bounded mesh runs and the workspace validator passed. A temporary obsolete-name fixture confirmed the naming guard rejects regressions and was removed.
