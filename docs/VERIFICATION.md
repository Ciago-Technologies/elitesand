# Verification of the R6 remediation revision

Executed locally on 31 October 2026 with rustc 1.99.1 stable, Linux x86_64. These are component/reference results, not production qualification.

| Command/check | Observed result |
| --- | --- |
| Original R6 probes before fixes | Seven core probes and one WAL probe passed, reproducing the reported defects/guards; original assertion text and reviewer logs retained under review/r6 |
| `cargo test --release --offline --workspace` | **112 passed, 0 failed**: includes 26 engine tests, 24 v30 regressions, 13 R5 regressions, 10 corrected R6 core tests, two WAL regression targets and three compile-fail doctests |
| `cargo test --release --offline --workspace --all-features` | **140 passed, 0 failed**; additional historical defect assertions are opt-in evidence, not remediation proof |
| `cargo fmt --all -- --check` | Passed |
| `python3 scripts/validate_workspace.py` | Passed: crate boundaries/DAG, opt-in legacy API, source listing, local links, vendor inventory, 39 R5 and 7 R6 dispositions, control-field privacy |
| `cargo clippy --workspace --all-targets --offline` | Completed successfully with warnings; no warning-free gate claimed |
| `python3 scripts/check_mesh_progress.py` | **1,000** cached-tail and **25** concurrent-grid runs passed in independently timed child processes |
| `cargo run --release --offline -p coagomem-core --example layout` | SlotHeader=128, Label=176, CoreControl=256, VaultLoc=56, Rust ExtentImage=64, RefToken=24, FrameValue=320 bytes. Image wire header=128 bytes |
| Vendor comparison | All 43 vendored package trees match their original crate archives and recorded SHA256 inventory |
| Extracted R6 upload ZIP | **140 all-feature tests passed** fully offline with a new empty CARGO_HOME and separate target directory; extracted workspace/documentation validator passed |
| Local benchmark | Executed on current source: bitmap 1,036 ns/query, scalar 4,609,543 ns/query, snapshot lookup 27 ns/call. See [scope and raw evidence](ECOSYSTEM_BENCHMARK.md) |

Label reader payload adds up to 64 bytes and allocator overhead. Dirty arrays now cost 33 bytes per slot with the checkpoint-barrier flag. Core controls are shared once across classes; the host must pass that same allocation to its WAL. Replay budgets charge both hash and heap storage. These are layout/arithmetic checks, not process RSS or million-user measurements.

R6 tests now check cross-class durability/fencing and other-core availability, mid-read fencing, denial of unsigned derived Fact content plus successful fresh signing, joint journal/anchor restore with fresh incarnations, approval TTL/quota/expiry and restart binding, previous/future image epochs, trusted metadata mismatch, absent versus corrupt images, retained/truncated WAL, route publication only after replay, replay abort, and hot-tenant barrier progress/failure. Existing query, mesh, lifecycle and simulated WAL recovery tests remain present.

The R6 reviewer's Miri subset and aarch64 compile logs apply to the preceding R5 workspace. They are attributed external evidence under review/r6/logs, not checks executed for these new changes. Only the stable toolchain is installed here. No current Miri/loom/Shuttle, ARM execution, long fuzzing, physical power cuts, full service transaction/replay certification, SQL comparison or one-million-user qualification was performed. Those gates remain open.

See [R6_REMEDIATION.md](R6_REMEDIATION.md) for current guarantees, breaking API changes and integration limits.
