# CiagoMem — SLAE

**Unified State-Log Actor Engine · CiaQL · Transactional state and AI memory**

CiagoMem — SLAE, also called **The Store**, is a Rust systems project for actor state, durable mutation logs, and provenance-aware AI working memory. Its architecture combines a Linux-native thread-per-core design with tenant isolation, asynchronous persistence, and deterministic multi-actor coordination.

The **0.3.0 engineering workspace** combines the P0 components with a corrected version of the v3.0 reference kernel supplied in the production-readiness review. The [corrected technical whitepaper](../ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md) distinguishes implementation evidence from architectural requirements.

> **Status:** Actor-native reference query execution, component libraries, security regressions and I/O simulation. A complete server, integrated transaction recovery, replication, and production qualification remain open. See the [R6 remediation ledger](docs/R6_REMEDIATION.md) and [earlier ledger](docs/REVIEW_REMEDIATION.md).

## Architecture

| Layer | Responsibility | Available components |
| --- | --- | --- |
| Actor state | Validated snapshots and in-place mutations | Word-atomic seqlocks, generational handles, arenas and completion-gated lifecycle |
| Durability | Sequential logging and checkpoints | CRC32C envelopes, group commit, A/B checkpoints and seeded I/O simulation |
| Query and policy | Typed queries and tenant-scoped execution | CiaQL execution, bitmap/zone-map filtering, joins, hybrid search and grouped analytics |
| Security | Authenticated capabilities and provenance | HMAC-SHA256 capabilities, Ed25519 provenance/approvals, replay checks and explicit readers |

One million **registered** tenants on a 16 GiB node is a design target. Resident working-set capacity and simultaneous agent sessions are separately bounded. Latency and write-amplification figures require workload and hardware qualification.

## Workspace

```text
ciagomem-slae/
├── Cargo.toml / Cargo.lock
├── coagomem-core/src/
│   ├── lib.rs              # Reviewed v3 component API and module wiring
│   ├── security.rs         # Labels, references and durable identity reservations
│   ├── policy.rs           # Verified-session ownership and destination checks
│   ├── dirty.rs            # Preallocated O(1) first-dirty FIFO
│   ├── calvin.rs           # Owner-local, message-tagged vote table
│   ├── seqlock.rs / slot.rs / mesh.rs
│   ├── io.rs / uring.rs / lifecycle.rs
│   └── legacy*.rs          # v2.5 API; optional legacy-audit feature only
├── coagomem-wal/src/        # WAL v2 envelopes, CRC32C and group commit
├── coagomem-ql/src/         # Typed lexer, parser and structural validation
├── ciagomem-crypto/src/     # Standard MACs, Ed25519 provenance and approvals
├── ciagomem-engine/         # Query runtime, schema images, indexes, joins and cascades
├── tests/                  # Corrected v3 regressions and Linux export checks
├── docs/REVIEW_REMEDIATION.md
├── scripts/validate_workspace.py
├── review/v30/             # Original ZIP source/assertions retained as text
├── vendor/                 # Unmodified upstream dependencies and licenses
└── LICENSE
```

The parser has no storage or cryptographic dependency. Core depends on the parser and crypto crates; WAL depends on core. Core currently compiles the dependency-free envelope source located in the WAL directory, which WAL re-exports. Keep this source layout intact until the format/types crate is extracted.

## Build and test

Use **Rust 1.89 or newer** and Cargo. Standard-library file locking and an independently durable identity rollback anchor are required by the incarnation journal. The tested toolchain is Rust 1.99.1. Linux is the primary target for `io_uring` and sealed memfd checks; no cross-platform qualification is claimed.

From this directory:

```sh
cargo build --release
cargo test --release
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
```

The validator also checks the whitepaper located alongside this workspace in the supplied project directory. The release profile uses `opt-level = 3` and `panic = "abort"`; the test harness uses its own panic behavior.

Focused checks:

```sh
cargo test --release -p coagomem-core --test v30_regressions
cargo test --release -p coagomem-core --test export_window
cargo test --release -p coagomem-ql
cargo test --release -p coagomem-wal
cargo test --release -p ciagomem-crypto
cargo test --release -p ciagomem-engine
cargo run --release -p ciagomem-engine --example benchmark
```

Added cryptographic and execution dependencies are vendored for offline builds, with upstream licenses and archive hashes retained. Existing libc/io-uring dependencies must be cached or fetched normally. This library workspace does not provide a server executable or deployment command.

## CiaQL

The default parser is available as `coagomem_ql::parse` and `coagomem_core::parse_ciaql`. Both produce the same typed query representation. Shorthand and `|>` pipelines share the grammar:

```text
@0x0008F421
where state.account.status == "ACTIVE" and state.wallet.credits >= 50
mutate {
  state.wallet.credits -= 50,
  state.metrics.api_calls += 1
}
select {
  remaining_credits: state.wallet.credits,
  schema_version: meta.schema_version
}
take 5
```

Parsing constructs an AST. Before execution, call `validate_query` using a freshly verified macaroon, an authoritative actor-owner resolver and explicit destination rules. Foreign actors are rejected; operator scans remain tenant-scoped; fleet backup fails closed; mutations require write permission. TAKE may be followed only by one final EXPORT. Search `top_k` is limited to 1–1,000, and numeric-vector similarity retains vector, metric and threshold.

The new `ciagomem_engine::Engine` executes the supported query subset and applies authorization internally. It adds reference joins with aliases, registered link constraints, strict/chunked cascades, FlatBuffers images, explicit owner-side schema upgrades, bitmap and SIMD/zone-map filters, BM25/SQ8 search, and grouped analytics. See [the detailed ecosystem design](docs/ACTOR_NATIVE_QUERY_ENGINE.md) for supported semantics and limits.

Runtime vector bindings, tenant-name aliases, general SQL joins, schema administration commands and full durable multi-core query execution remain open. Unsupported syntax and stage combinations return errors.

## Security and persistence

- **Authenticated capabilities:** RustCrypto HMAC-SHA256 replaces the home-grown ARX function. Macaroons require signed tenant, expiry, UID, session, audience, boot epoch, operation and key epoch caveats. Ed25519 provenance and approvals bind content, frame field, session, boot epoch, nonce, tool and sink; verifiers hold only public keys.
- **Explicit readers:** Labels contain bounded sets of full 128-bit reader IDs, avoiding modulo-64 tenant aliasing. LLM outputs inherit context restrictions; Q-LLM output is External.
- **Persistent identity:** An exclusive hash-chained journal restores tenant mappings and deletion history. New incarnations use fresh OS randomness. A mandatory independent rollback anchor separates acknowledged records from recoverable unacknowledged tails. A deployable durable anchor and WAL/backup integration remain open.
- **Bounded progress:** Overflow returns the rejected item at its hard cap. Cumulative consumption acknowledgements reject duplicate inflation; control/data mesh capacity is separate. Dirty tracking uses a preallocated intrusive FIFO and an explicit owner checkpoint barrier; stale completions cannot release pins. Voting requires exclusive owner access, eliminating the slot-tag/RMW race.
- **Read semantics:** The canonical arena uses deadlines and report stalled writers without declaring them dead. The clock currently uses `Instant`; calibrated hardware counters remain a qualification task.
- **Agent exports:** Linux windows are sealed and tested against writable upgrades. A shared byte/session budget reserves capacity before allocation and releases it on failure/drop. Session transport and publication remain unimplemented.

Root exports now alias the canonical persistence-wired arena/seqlock types. Arena constructors require node-shared durability/fence controls. Reads check full incarnations; mutation writes reject equal/regressing LSNs. Extent image v3 accepts earlier epochs under trusted recovery bounds and distinguishes absent/corrupt images. Single-copy recovery requires retained WAL coverage; routes stay unpublished until required replay completes. WAL failure fences the core and reports indeterminate outcomes; late completions cannot publish durability. Durable deletion and recovery orchestration remain open. Component tests do not establish end-to-end tenant isolation or ACID for a running service.

## Review and validation

The supplied v3 ZIP tests passed when defects were present. Their corrected counterparts in [v30_regressions.rs](tests/v30_regressions.rs) assert rejection or corrected behavior. Original source and assertions are retained under `review/v30` for provenance.

The older v2.5 implementation is available only through the nondefault `legacy-audit` feature:

```sh
cargo test --release --all-features
```

This enables historical exploit reproductions under `coagomem_core::legacy`. A passing historical reproduction confirms a known flaw; it is not remediation evidence and should not be used as an application authorization API.

Read the [actor-native engine design](docs/ACTOR_NATIVE_QUERY_ENGINE.md) for the five capability scopes, the [benchmark report](docs/ECOSYSTEM_BENCHMARK.md) for measured internal-path comparisons, and the [verification report](docs/VERIFICATION.md) for executed checks.

The [R6 remediation ledger](docs/R6_REMEDIATION.md) covers all seven new findings and affected earlier guarantees to source and tests and tracks remaining integration work. Simulation is not a substitute for physical power-loss tests, clock calibration, Miri/loom/fuzzing, transaction-history analysis or production benchmarks. Existing P0 and upstream-vendored lint warnings remain; zero-warning production sign-off is not claimed.

## License

Copyright © 2026 **Ciago Technologies / Anuj Kumar Tiwari**. All rights reserved.

Project code and documentation are proprietary and confidential; see [LICENSE](LICENSE). Vendored third-party dependencies retain their own upstream licenses.
