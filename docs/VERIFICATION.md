# Verification of the R11 self-audit remediation

Local Linux x86_64 checks on 31 October 2026:

| Check | Observed result |
| --- | --- |
| Release workspace, offline | **165 default / 193 all-feature tests passed**, zero failures |
| New acceptance coverage | Eight R11 integration tests, one stale-generation unit test, five new compile-fail API tests |
| Real-ring visibility | R10 core/WAL tests rerun with `--nocapture`; three bodies explicitly skipped because io_uring is unavailable |
| Formatting and diff whitespace | Passed |
| Clippy, all targets and all features | Completed with warnings; not a warning-free gate |
| Mesh progress | 1,000 cached-tail and 25 concurrent FIFO bounded runs passed |
| Workspace/specification validator | Passed, including sections14–16 mirror comparison, native/wire layouts and required CI commands |
| Extracted review package | **193 all-feature tests passed offline with an empty Cargo home**; extracted workspace/spec validator passed |
| Hosted CI | Workflow updated; GitHub Actions was not executed locally |

The pass totals include successful harness returns from the three skipped hardware bodies. Compile-fail tests require inaccessible raw atomics, non-escaping read/write views and single-use pin tokens to be rejected by Rust. Existing R9/R10 submission unwind, teardown and SQ rejection checks still pass.

See [R11_REMEDIATION.md](R11_REMEDIATION.md) for API changes and costs. Raw local logs and the original pre-fix probe evidence are under `review/r11/`. Earlier sections below are historical results. No benchmarks were rerun: the access guards add per-slot memory and read-path work. Miri, Loom/Shuttle, fuzzing, ARM execution, real NVMe/power cuts and distributed operation remain unverified.

---

# Verification of the R10 remediation revision

Local Linux x86_64 checks on 31 October 2026:

| Check | Observed result |
| --- | --- |
| Pre-fix logical probes | Checkpoint-unwind and fenced-log non-quiescence defects reproduced on the current canonical-named tree |
| Release tests, offline | 151 default / 179 all-features harness successes, no failures; see real-ring qualification below |
| R10 tests with nocapture | Core 5 and WAL 4 harness successes; **three tests explicitly skipped real io_uring bodies** because io_uring is unavailable |
| Formatting | Passed after patch normalization |
| Safe exploit probe compile check | Original safe probe rejected: private queue and u64 storage token cannot be passed as typed u32 wake |
| Clippy, all workspace targets offline | Completed with warnings; no warning-free gate claimed |
| Mesh progress | 1,000 cached-tail + 25 concurrent-grid bounded runs passed |
| Layout example | SlotHeader128/alignment64, actual offsets asserted; ExtentImage descriptor64/alignment16 on this host |
| Specification validators | Target layouts, arithmetic, mirrors and current-example equality checked; no runtime implementation proof implied |

Only stable x86_64 Rust is installed. Local Miri, loom/Shuttle, ARM execution, fuzz campaigns, real NVMe/O_DIRECT, power cuts, distributed failure tests and warm restart were not run. The incoming report attributes Miri/aarch64 compile-only and buffered-file real-ring results to its own R10 patch/host; those are preserved evidence, not local results. Full application/cluster durability and million-active-user claims remain unqualified.

See [R10_REMEDIATION.md](R10_REMEDIATION.md) for corrections and boundaries. The prior R9 measurements below remain historical.

---

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
