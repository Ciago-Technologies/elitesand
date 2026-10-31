# Ciagomem — SLAE

**Actor state, CiaQL queries and provenance-aware AI memory for Rust applications.**

Ciagomem stores application data as tenant-scoped actors and provides an embedded query engine for updating state, following relationships, searching memory and computing aggregates. Its storage components use single-writer ownership, cross-core message lanes, write-ahead logs and checkpoints.

**Development release:** you can run and embed the in-memory engine today. The server interface and integration of durable engine writes and restart recovery are under development.

## Quick start

You need **Linux**, **Rust 1.89 or newer**, and **Cargo**. Python 3 is needed only for the workspace validation script. Dependencies are vendored, so build and run commands work offline after checkout and toolchain installation.

```sh
git clone https://github.com/Ciago-Technologies/CiagoMem.git
cd Ciagomem
cargo run --release --offline -p ciagomem-engine --example quickstart
```

Expected output:

```text
Created actor 1
Credits after update: 75
```

The example creates a local authenticated session, inserts an actor with 100 credits, subtracts 25 through CiaQL, and reads the updated state. It checks both the query result and actor snapshot.

Open [quickstart.rs](ciagomem-engine/examples/quickstart.rs) to adapt the complete example. It uses a fresh process-local key and boot epoch. Its `MemoryJournal` keeps data in memory; exiting the process discards that data.

## Write and query actor state

An actor has an ID, an incarnation and a tenant. Records hold typed fields; sessions determine which tenant and operations a caller can access.

The quick start uses these queries:

```text
@1 mutate {state.credits -= 25}

@1 select {credits: state.credits}
```

Parse a CiaQL string with `ciagomem_ql::parse`, then pass it and a verified session to `Engine::execute`. The engine checks tenant ownership, operation permissions, token expiry and boot scope. Use `Engine::snapshot` to retrieve an immutable actor version.

For a Rust application beside your checkout, add local path dependencies, adjusting the paths to your layout:

```toml
[dependencies]
ciagomem-engine = { path = "../Ciagomem/ciagomem-engine" }
ciagomem-core = { path = "../Ciagomem/ciagomem-core" }
ciagomem-ql = { path = "../Ciagomem/ciagomem-ql" }
```

Use the [complete quick start](ciagomem-engine/examples/quickstart.rs) for engine, clock and session initialization. For offline application development, add your application to this repository's Cargo workspace so its vendored dependency patches apply; an external application's dependency resolution is controlled by its own workspace.

## What you can build with the engine

- **Application state:** store typed actor records, apply mutations and read immutable snapshots.
- **Related entities:** follow actor references, execute bounded joins, register links and perform restricted or cascading deletions.
- **AI memory retrieval:** combine BM25 keyword search, SQ8 vector similarity and structured filters.
- **Analytics:** filter with bitmap and zone-map indexes, group records and compute aggregates.
- **Schema evolution:** use versioned FlatBuffers actor images and registered schema upcasters.
- **Tenant-aware workflows:** use scoped capabilities, explicit reader sets, signed provenance and action approvals.

See [Actor-native query engine](docs/ACTOR_NATIVE_QUERY_ENGINE.md) for supported syntax, consistency rules and execution limits. Unsupported operations return errors.

## Choose a crate

| Crate             | Use it for                                                                               |
| ----------------- | ---------------------------------------------------------------------------------------- |
| `ciagomem-engine` | Embedded actor storage, query execution, indexes, relationships and schema upgrades      |
| `ciagomem-ql`     | CiaQL parsing and typed query validation                                                 |
| `ciagomem-core`   | Authorization, actor arenas, messaging, lifecycle management and Linux export primitives |
| `ciagomem-wal`    | Group-commit logging, checksums and checkpoint formats                                   |
| `ciagomem-crypto` | Capability MACs, content digests and signed provenance/approvals                         |

Start with `ciagomem-engine` for application integration. The lower-level storage and security crates expose components for building a host runtime; they require the ownership and I/O contracts described in the [engineering specification](MASTER_SPECIFICATION.md).

## Current release scope

The embedded engine uses `MemoryJournal`, a bounded, non-durable journal. The WAL crate is available separately; a WAL-backed engine journal and complete restart replay are still required for persistent application storage.

This checkout provides Rust libraries and runnable examples. Network server endpoints, client SDKs, replication, production backup/restore and the complete reactor/node runtime remain under development. Use this release for local integration, experiments and workload evaluation. Production deployment requires completion and qualification of those services.

Performance depends on workload, indexes, working set and durability requirements. The [benchmark notes](docs/ECOSYSTEM_BENCHMARK.md) describe a local smoke measurement; they do not establish production latency, SQL speedups or a million-user capacity guarantee.

## Build and verify

Run from the repository root:

```sh
cargo build --release --offline
cargo test --release --offline --workspace
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
```

For the complete test set, including historical compatibility cases:

```sh
cargo test --release --offline --workspace --all-features
```

The `legacy-audit` feature is for historical vulnerability reproductions. Keep it disabled in application integrations.

## Documentation

- [Quick start source](ciagomem-engine/examples/quickstart.rs)
- [Actor-native engine guide](docs/ACTOR_NATIVE_QUERY_ENGINE.md)
- [Technical whitepaper and engineering specification](MASTER_SPECIFICATION.md)
- [Verification results](docs/VERIFICATION.md)
- [Engineering status and open integration requirements](docs/R9_REMEDIATION.md)

## License

Copyright © 2026 **Ciago Technologies / Anuj Kumar Tiwari**. All rights reserved.

Ciagomem source and documentation are proprietary; use is subject to [LICENSE](LICENSE). Vendored third-party dependencies retain their upstream licenses.
