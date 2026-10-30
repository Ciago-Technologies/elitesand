# Verification of the R5 remediation revision

Executed on 31 October 2026 with rustc 1.99.1 on Linux x86_64. Results apply to component/reference execution, not a production service.

| Command/check | Observed result |
| --- | --- |
| `cargo test --release --offline --workspace` | **100 passed, 0 failed**, including 26 ecosystem tests, 24 corrected v3 regressions, 13 R5 regressions, a corrected WAL regression and two compile-fail label doctests |
| `cargo test --release --offline --workspace --all-features` | **128 passed, 0 failed**; includes historical vulnerability reproductions behind legacy-audit |
| `cargo fmt --all -- --check` | Passed |
| `python3 scripts/validate_workspace.py` | Passed: crate DAG, parser/crypto/engine boundaries, opt-in legacy API, embedded source, local links, vendor inventory and all 39 R5 finding dispositions |
| `cargo clippy --workspace --all-targets --offline` | Completed successfully with warnings; no warning-free gate claimed |
| `python3 scripts/check_mesh_progress.py` | **1,000** deterministic interleaving runs and **25** concurrent grid runs passed, each in an independently timed child process |
| `cargo run --release --offline -p ciagomem-core --example layout` | SlotHeader=128, Label=160, CoreControl=384, VaultLoc=56, ExtentImage Rust struct=64, RefToken=24 bytes on this target; image wire header is 128 bytes |
| Vendor comparison | All **43 packages** byte-for-byte equal to retained original crate archive contents; archive SHA256 inventory checked. Original archives came from the Cargo cache or direct upstream downloads. |
| Extracted upload ZIP | All **128 all-feature tests passed** after extraction with empty CARGO_HOME and a separate target directory, fully offline; extracted documentation validator also passed |
| Empty-cache metadata | `cargo metadata --offline` succeeded with a new empty CARGO_HOME; all resolved package sources are local paths |
| Local smoke benchmark | Executed; see [ECOSYSTEM_BENCHMARK.md](ECOSYSTEM_BENCHMARK.md) and source-hashed JSON |

Label reader storage adds up to 64 bytes of allocated u128 payload, plus allocator overhead, to the 160-byte Label object. Serialized proofs, action permits and retained journal images cost additional memory. Do not reuse the old 40-byte label estimate. Layout is target/compiler dependent.

Corrected tests cover tenant/session/audience checks, destination roots and filesystem traversal/symlinks, signed provenance and action scope, replay/reader rejection, independent-anchor identity rollback and prefix corruption, unacknowledged tails, checkpoint pin coverage, image incarnation/epoch/tombstones, indeterminate WAL fencing, duplicate lifecycle CQEs, equal-LSN rejection, duplicate credits, vector bounds and deep AST validation/destruction. The earlier actor-native join/schema/search/analytics suite remains present.

The mesh consumer refreshes the published tail before dropping pending state; finite progress deadlines and the subprocess runner prevent a deadlock from hanging verification indefinitely. Repetition is evidence for these tested schedules, not a weak-memory proof.

Miri, loom, aarch64 execution, long fuzz/property campaigns, hardware power cuts, integrated transaction/replay certification, authenticated server transport, SQL comparison and one-million-tenant capacity qualification were **not** performed for this revision. The independent review describes its own checks on the older snapshot; those are not our current results. No production sign-off is claimed.

Historical adversarial assertions pass when old defects exist; they are retained solely as compatibility/provenance evidence. See [R5_REMEDIATION.md](R5_REMEDIATION.md) for every finding and remaining gates.
