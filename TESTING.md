# Testing and qualification

**Current:** run the Rust workspace and document checks below. **Target:** schema/SDK/container and production failure suites in specification 14.12,15.12 and17.8. No ephemeral published server container is provided by this checkout.

## Current checks

From `ciagomem-slae/`:

```sh
cargo test --release --offline --workspace
cargo test --release --offline --workspace --all-features
cargo fmt --all -- --check
cargo clippy --offline --workspace --all-targets --all-features
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
python3 scripts/check_mesh_progress.py
cargo run --release --offline -p ciagomem-engine --example quickstart
```

All-feature tests include optional historical vulnerability demonstrations. Clippy warnings are currently visible, not a warning-free gate. Run hardware-related tests with `--nocapture` and count bodies reporting SKIPPED separately from Cargo harness successes. Simulator tests do not qualify DMA, NVMe power-loss persistence or ARM execution.

## Target schema and application tests

After installing the application-pinned qualified compiler:

```sh
flatc --conform schemas/app_schema.v1.fbs schemas/app_schema.v2.fbs
```

This is a developer precheck; it does not replace bounded BFBS/document verification, catalog compatibility, upcaster correctness, resource quotas or replay tests. Use explicit stable field IDs; deprecate removed fields; append compatible optional fields. Do not change scalar types/defaults or struct layouts in place. Test rejected migrations and exact class boundaries as well as valid upgrades.

Future clients use fresh authorized test namespaces and exclusive temporary identity journals. Reuse the same identity for retry/restart tests; creating a new identity would hide duplication defects. Test concurrent stale versions, conditional balance rejection, arithmetic overflow, arrays at capacity, corrupt schema/root offsets, cross-family authorization and link/delete races.

## Required future suites

| Suite group | Required evidence |
| --- | --- |
| Application grammar | Every target reference example; invalid operators/aliases/types; byte spans and bounds |
| Conditional CRUD | Same-version races, terminal result recovery, lost ACK and no duplicate debit |
| Configuration | Precedence, invalid/unknown settings, memory arithmetic, TLS/clock/anchor readiness failures |
| Capability codec | Cross-language known answers, attenuation, malformed lengths, scope widening, clock/key/boot changes |
| SDK retries/diagnostics | Partial frames, identity-file crash boundaries, Busy/Indeterminate/RetryTooOld, redaction/logger failure |
| General workloads | CRUD/session/SaaS/AI/telemetry models, transfer conservation, cascades and exact retrieval checks |
| Release/operations | Actual image/client installation, graceful genuine CQEs, backup/recovery, upgrades and native export adapter |

These are implementation requirements, not claims that corresponding tests already exist. Match each milestone to executable acceptance tests before marking it delivered.

## Performance qualification

Record schemas/vector dimensions, dataset, key skew, registered/resident/concurrent counts, read/write mix, hit rate, durability, batching, resource caps, compiler, CPU/kernel/storage, affinity and frequency policy. Report throughput and p50/p99/p99.99 with sample counts. Measure separately:

- Resident engine snapshot, including R11 guard work.
- Getter access to an already-published immutable export.
- Export materialization/FD transfer and full request latency.
- Cold fault/read/replay and admission under pressure.
- Qualified local WAL persistence ACK and current-term quorum ACK.

Earlier specification figures (resident200 ns, published getter300 ns, local durable50 µs and cold fault250 µs) are objectives requiring workload/hardware evidence, not release guarantees. Network durable latency is not an in-memory lookup measurement. Differential pushdown tests must return identical authorized results.60-second ramps are smoke evidence only; sustained qualification must expose checkpoint/compaction, spool pressure, throttling and failure recovery.

See [recorded verification](docs/VERIFICATION.md) for observed local results and skipped checks. Miri/model checking, fuzzing, ARM and real power-cut/storage qualification require their own evidence.

## Current orchestration tests

```sh
cargo test --release --offline -p ciagomem-engine --test memory
cargo run --release --offline -p ciagomem-engine --example memory_orchestration
```

These exercise the in-process scoped broker, taint/reader propagation, transitive invalidation, TTL/rollback, input/output packing, publication retries and journal rejection. They do not call paid models, prove summary grounding, measure token savings or establish restart-safe task recovery. Specification18 lists the remaining service, adapter, provider and adversarial requirements.
