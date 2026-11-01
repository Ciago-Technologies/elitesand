# Ciagomem — SLAE

> **Single-node service update:** Linux TLS1.3/mTLS reactor hosting and durable client retries are implemented in the fixed-Record profile. See [specification20](MASTER_SPECIFICATION.md#20-implemented-single-node-tls-reactor-and-durable-client-retries) and [source deployment](INSTALL.md#run-the-current-single-node-service). BFBS families, Go/TypeScript SDKs and the distributed runtime remain target work; hardware qualification remains required.

Ciagomem is an actor-oriented Rust storage project for application state and AI memory. Its target combines custom schema families, transactional mutations, text/vector retrieval and provenance controls. Users, sessions, orders, telemetry and agent memories are all intended workloads; gaming and wallets are examples.

## Start using the current Rust engine

Install Rust with Cargo, then run from this directory:

```sh
cargo run --release --offline -p ciagomem-engine --example quickstart
```

The example creates an actor, subtracts 25 credits atomically and reads 75 back. It uses the embedded reference engine and **non-durable MemoryJournal**. Its full setup, authentication and error handling are in [quickstart.rs](ciagomem-engine/examples/quickstart.rs).

```rust
// After initializing Engine and an authorized session as in the complete quickstart:
let update = ciagomem_ql::parse("@1 mutate {state.credits -= 25}")?;
engine.execute(&update, &session)?;
let query = ciagomem_ql::parse("@1 select {credits: state.credits}")?;
let rows = engine.execute(&query, &session)?;
```

For a local Rust application, use path dependencies on `ciagomem-engine`, `ciagomem-core` and `ciagomem-ql`; see [installation](INSTALL.md). Native Linux I/O/export components have additional kernel and storage qualification requirements.

## Worker memory handoffs

A bounded Rust broker can delegate exact source actors to a worker session and publish a cited summary for the controller. Raw evidence remains on the worker path; source changes invalidate cached descendants. It preserves External taint and enforces byte/lease/task budgets. Run the provider-free example:

```sh
cargo run --release --offline -p ciagomem-engine --example memory_orchestration
```

See [the orchestration use case](USECASE.md#5-sub-agent-memory-orchestration-the-zero-token-waste-pattern). Model execution, namespace/network SDK integration and durable task recovery remain planned; summaries still consume model tokens.

## Current release and target architecture

This checkout contains five library crates, runnable examples and adversarial tests. The Linux server/issuer CLI and source Rust client are available in the fixed-Record profile. Container releases, runtime BFBS collections, general document arrays, repair integration, fully asynchronous actor scheduling and published Go/TypeScript/Rust SDKs remain target milestones. The [capability matrix](MASTER_SPECIFICATION.md#1411-ground-truth-capability-matrix-and-ordered-implementation-roadmap) defines their dependencies. No registry image or SDK publication is established here.

The embedded engine offers reference scalar queries/mutations, text/vector indexing, relationship/deletion logic and authorization components. Its MemoryJournal does not provide restart-safe application storage. DurableJournal joins WAL/checkpoint/replay and a durable retry ledger through a storage owner, with a separate pinned TLS/io_uring network reactor. Production deployment requires native hardware and failure qualification; earlier benchmark results do not establish million-user capacity or guaranteed latency.

## Documentation

| Guide | Contents |
| --- | --- |
| [INSTALL.md](INSTALL.md) | Current source installation; future service release requirements |
| [ABOUT.md](ABOUT.md) | Architecture, implemented primitives and planned layers |
| [EXAMPLES.md](EXAMPLES.md) | Runnable Rust entrypoint and target schema/SDK workflows |
| [USECASE.md](USECASE.md) | General application, AI, SaaS, wallet/gaming and local IPC models |
| [CIAQL_REFERENCE.md](CIAQL_REFERENCE.md) | Current syntax versus consolidated target grammar |
| [SECURITY.md](SECURITY.md) | Capability/provenance boundaries and planned transport policy |
| [CONFIGURATION.md](CONFIGURATION.md) | Current library configuration and proposed host settings |
| [DEBUGGING.md](DEBUGGING.md) | Current diagnostics and target safe retries/logging |
| [TESTING.md](TESTING.md) | Runnable checks, future acceptance suites and performance methodology |
| [MASTER_SPECIFICATION.md](MASTER_SPECIFICATION.md) | Authoritative engineering contracts, sections14–17 |
| [Verification evidence](docs/VERIFICATION.md) | Observed checks and limitations |

## Verify the checkout

```sh
cargo test --release --offline --workspace
cargo test --release --offline --workspace --all-features
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
```

`legacy-audit` enables historical vulnerability reproductions; leave it disabled in application integrations. See [LICENSE](LICENSE) for the proprietary terms. This README does not grant a separate commercial license.

## Target scaling and multi-model execution

The target Hub-and-Spoke design keeps bounded point operations on owner reactors and moves expensive joins, aggregations and ANN work to budgeted Query Workers using immutable snapshots. Epoch-fenced gateway routing and pooled streams distribute admitted work across nodes. The one-million-outstanding-request objective is cluster-wide and requires workload qualification; the local200-ns read objective excludes networking, authorization, queries and persistence.

Each storage node retains a complete16-GiB budget; gateway/query/replica hosts add separately accounted resources. Historical time-series, columnar data and vector structures require planned immutable segment manifests on NVMe/object storage. The fifteen-modality map describes independent target contracts, not compatibility with fifteen existing databases. See [specification15.13–15.18](MASTER_SPECIFICATION.md#1513-horizontal-scaling-and-the-one-million-request-qualification-contract).

The next implementation priority is the existing single-node durability/recovery, bounded authenticated host and consistent index-publication path. Broad SQL/time-series/graph engines remain extended scope.

The scaling audit additionally requires distributed viral-actor reads, bounded asynchronous view maintenance with consistent cuts, and parked cold faults with reserved completion resources. Full requirements and workload-specific database replacement gates are in specification15.13–15.18; a10–30% hot population and pooled connections do not remove non-arena memory costs.

## Run the durable embedded host

```sh
cargo run --release --offline -p ciagomem-engine --example durable_host
```

The example persists a mutation, checkpoints and reads the recovered value after reopening with a fresh boot epoch. It uses a private temporary store and random in-memory demo keys. Native io_uring must be explicitly enabled on the final storage-owner thread; ordinary example mode uses buffered writes plus sync_data. This example exercises the embedded host. The separate fixed-Record TLS service and durable retry workflow are described below.

## Run the single-node service

```sh
cargo build --release --offline -p ciagomem-engine --bin ciagomem-server
./target/release/ciagomem-server --help
```

Linux and OpenSSL3.5 LTS (>=3.5.9, including development libraries) are required. Follow [INSTALL](INSTALL.md#run-the-current-single-node-service) to provision TLS principals, persistent keys, data and client identity. Native mode requires two allowed CPUs and io_uring; explicit loopback test mode keeps TLS/authentication/durable retries enabled. The runnable [Rust network client](ciagomem-engine/examples/network_client.rs) stages a request on disk before sending and resumes the same intent after a disconnect/restart. Source API: [client.rs](ciagomem-engine/src/client.rs).

This profile serves bounded fixed ActorImages and scalar CiaQL operations. BFBS families, general arrays, cluster replication and analytical worker isolation remain target work. Tests do not certify power-loss durability, million-user capacity or network latency.
