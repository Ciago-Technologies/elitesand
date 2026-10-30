# CiagoMem — SLAE

**Unified State-Log Actor Engine · CiaQL · Transactional state and AI memory**

CiagoMem — SLAE, also called **The Store**, is a Rust systems project for actor state, durable mutation logs, and provenance-aware AI working memory. Its architecture combines a Linux-native thread-per-core design with tenant isolation, asynchronous persistence, and deterministic multi-actor coordination.

The **0.3.0 engineering workspace** combines the P0 components with a corrected version of the v3.0 reference kernel supplied in the production-readiness review. The [corrected technical whitepaper](../ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md) distinguishes implementation evidence from architectural requirements.

> **Status:** Component libraries with security regressions and I/O simulation. A complete server, integrated transaction recovery, replication, and production qualification remain open. See the [remediation ledger](docs/REVIEW_REMEDIATION.md).

## Architecture

| Layer | Responsibility | Available components |
| --- | --- | --- |
| Actor state | Validated snapshots and in-place mutations | Word-atomic seqlocks, generational handles, arenas and completion-gated lifecycle |
| Durability | Sequential logging and checkpoints | CRC32C envelopes, group commit, A/B checkpoints and seeded I/O simulation |
| Query and policy | Typed queries and tenant-scoped authorization | Literal-aware CiaQL AST, ownership validation and operation scopes |
| Security | Authenticated capabilities and provenance | HMAC-SHA256, caveat chains, endorsements, explicit reader IDs and label checks |

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
├── ciagomem-crypto/src/     # Standard MAC provider and signed capabilities
├── tests/                  # Corrected v3 regressions and Linux export checks
├── docs/REVIEW_REMEDIATION.md
├── scripts/validate_workspace.py
├── review/v30/             # Original ZIP source/assertions retained as text
├── vendor/                 # Unmodified cryptographic dependencies and licenses
└── LICENSE
```

The parser has no storage or cryptographic dependency. Core depends on the parser and crypto crates; WAL depends on core. Core currently compiles the dependency-free envelope source located in the WAL directory, which WAL re-exports. Keep this source layout intact until the format/types crate is extracted.

## Build and test

Use **Rust 1.89 or newer** and Cargo. Standard-library file locking is required by the incarnation journal. The tested toolchain is Rust 1.99.1. Linux is the primary target for `io_uring` and sealed memfd checks; no cross-platform qualification is claimed.

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
```

Added cryptographic dependencies are vendored for offline builds, with upstream licenses and archive hashes retained. Existing libc/io-uring dependencies must be cached or fetched normally. This library workspace does not provide a server executable or deployment command.

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
  tx_epoch: meta.last_tx_seq
}
take 5
```

Parsing constructs an AST. Before execution, call `validate_query` using a freshly verified macaroon, an authoritative actor-owner resolver and explicit destination rules. Foreign actors are rejected; fleet scans require operator scope; mutations require write permission. TAKE may be followed only by one final EXPORT. Search `top_k` is limited to 1–1,000, and numeric-vector similarity retains vector, metric and threshold.

Runtime vector bindings, alias resolution, joins, links, schema commands, query execution and result-byte budgets remain future work. Unsupported syntax returns an error.

## Security and persistence

- **Authenticated capabilities:** RustCrypto HMAC-SHA256 replaces the home-grown ARX function. Macaroons require signed tenant, expiry, UID, operation and epoch caveats. Endorsements bind tenant, sources and expiry.
- **Explicit readers:** Labels contain bounded sets of full 128-bit reader IDs, avoiding modulo-64 tenant aliasing. LLM outputs inherit context restrictions; Q-LLM output is External.
- **Durable reservations:** An exclusive append-only journal syncs incarnation reservations before returning them and rejects corrupt or incomplete tails. Persistent tenant lookup mappings and WAL-superblock integration remain open. Retain this authoritative journal with backups.
- **Bounded progress:** Overflow returns the rejected item at its hard cap. Credits are clamped and excess returns counted. Dirty tracking uses a preallocated intrusive FIFO. Voting requires exclusive owner access, eliminating the slot-tag/RMW race.
- **Read semantics:** Both arenas use deadlines and report stalled writers without declaring them dead. The clock currently uses `Instant`; calibrated hardware counters remain a qualification task.
- **Agent exports:** Linux windows are sealed and tested against writable upgrades. A shared byte/session budget reserves capacity before allocation and releases it on failure/drop. Session transport and publication remain unimplemented.

The reviewed heap arena and P0 persistence arena expose distinct types. Both reject LSN regression before mutation and use monotonic durable watermarks, but the P0 disk format still needs incarnation fields and deletion tombstones. Component tests do not establish end-to-end tenant isolation or ACID for a running service.

## Review and validation

The supplied v3 ZIP tests passed when defects were present. Their corrected counterparts in [v30_regressions.rs](tests/v30_regressions.rs) assert rejection or corrected behavior. Original source and assertions are retained under `review/v30` for provenance.

The older v2.5 implementation is available only through the nondefault `legacy-audit` feature:

```sh
cargo test --release --all-features
```

This enables historical exploit reproductions under `coagomem_core::legacy`. A passing historical reproduction confirms a known flaw; it is not remediation evidence and should not be used as an application authorization API.

The [remediation ledger](docs/REVIEW_REMEDIATION.md) maps findings to source and tests and tracks remaining integration work. Simulation is not a substitute for physical power-loss tests, clock calibration, Miri/loom/fuzzing, transaction-history analysis or production benchmarks. Existing P0 and upstream-vendored lint warnings remain; zero-warning production sign-off is not claimed.

## License

Copyright © 2026 **Ciago Technologies / Anuj Kumar Tiwari**. All rights reserved.

Project code and documentation are proprietary and confidential; see [LICENSE](LICENSE). Vendored third-party dependencies retain their own upstream licenses.
