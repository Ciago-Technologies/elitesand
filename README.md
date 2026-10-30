# CiagoMem - SLAE

Rust 2024 workspace assembled from `slae-p0` and `ciagomem-v25-audit-harness/v25-audit`. The exploratory `slae-patterns.zip` is unused. Original extracted sources are unchanged.

- `coagomem-core`: original v2.5 master library and hardening patterns, plus the P0 seqlock, mesh, slots, io_uring, Calvin, simulation, and lifecycle modules.
- `coagomem-wal`: v2 envelopes, CRC32C, and P0 group-commit log.
- `coagomem-ql`: the P0 literal-aware query parser.
- `tests`: original audit exploit reproductions and Linux sealed memfd export checks, explicitly registered as core integration targets.

## Build and test

Requires Rust 1.85 or newer. From this directory:

```sh
cargo test --release
```

Linux-specific io_uring and export checks require a compatible kernel. The simulated I/O tests do not establish real device durability.

## Assembly details and limitations

The audit master `lib.rs` is a monolithic implementation, not just module declarations. Its root APIs (including `parse_ciaql`) are preserved for the original audit tests and hardening module. The P0 modules expose distinct APIs under `coagomem_core::{seqlock, mesh, slot, calvin, ...}`; the P0 parser is `coagomem_ql::parse`. These implementations are not automatically interchangeable, and the audit hardening helpers are not automatically applied to the P0 modules.

The P0 lifecycle depends on `ExtentImage`. To avoid a circular core/WAL dependency, core compiles the dependency-free envelope source located at `coagomem-wal/src/envelope.rs`, and WAL re-exports that same module. WAL group-commit imports of core I/O and slot types are updated for the crate split. WAL and QL include the library entry points required by Cargo.

The adversarial suite intentionally reproduces known vulnerabilities (some tests assert vulnerable behavior). A passing suite verifies faithful assembly and those reproductions; it does not certify security or production readiness. Production cryptography, integration of hardening patterns, and remediation of audit findings remain separate engineering work.

Proprietary / All Rights Reserved; see LICENSE.
