# Ciagomem — SLAE

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

## Current release and target architecture

This checkout contains five library crates, runnable examples and adversarial tests. A production server/container/CLI, runtime BFBS collections, general document arrays, complete durable application replay and external Go/TypeScript/Rust clients remain target milestones. The [capability matrix](MASTER_SPECIFICATION.md#1411-ground-truth-capability-matrix-and-ordered-implementation-roadmap) defines their dependencies. No registry image or SDK publication is established here.

The embedded engine offers reference scalar queries/mutations, text/vector indexing, relationship/deletion logic and authorization components. Its MemoryJournal does not provide restart-safe application storage. WAL, io_uring and checkpoint libraries are separate components awaiting full host integration. Production deployment requires those dependencies and failure qualification; earlier benchmark results do not establish million-user capacity or guaranteed latency.

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
