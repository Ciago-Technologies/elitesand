# Installation and deployment

**Current:** Rust libraries and runnable examples. **Target:** a deployable server, CLI, container image and network SDKs. The target release checklist is in specification 17.7; the workspace does not currently ship those artifacts.

## Build and run locally

Install a Rust toolchain supporting edition2024 and Cargo. From `ciagomem-slae/`:

```sh
cargo build --release --offline --workspace
cargo run --release --offline -p ciagomem-engine --example quickstart
```

Dependencies are provided under vendor with upstream licenses. The example uses heap/reference components and MemoryJournal. It is suitable for local integration and behavior evaluation, not persistent application storage. Kernel-backed io_uring/memfd tests may skip or require Linux capabilities unavailable on the host; read their output.

For an application beside this checkout:

```toml
[dependencies]
ciagomem-engine = { path = "../ciagomem-slae/ciagomem-engine" }
ciagomem-core = { path = "../ciagomem-slae/ciagomem-core" }
ciagomem-ql = { path = "../ciagomem-slae/ciagomem-ql" }
```

Use the complete [embedded quickstart](ciagomem-engine/examples/quickstart.rs) for capability issuance, initialization and typed error handling. Do not assume network clients are installed by adding these dependencies.

## Target schema and client build tools

The future collection workflow uses an application-pinned FlatBuffers compiler and language runtimes tested against the same reflection format. After installing a qualified `flatc`, application CI will generate:

```sh
flatc --binary --schema -o build app_schema.fbs
flatc --rust -o generated/rust app_schema.fbs
flatc --go -o generated/go app_schema.fbs
flatc --ts -o generated/ts app_schema.fbs
```

These compile application artifacts; they do not add runtime SchemaCatalog support to today's engine. See [target examples](EXAMPLES.md) and specification 14.2/17.7. Network packages must be published and interoperably tested before installation commands for them become release instructions.

## Target production deployment prerequisites

Before a production Docker/Compose guide can be followed, the release must provide:

1. Executable host, tested Dockerfile and signed immutable image digest with SBOM/licenses and exact supported platform matrix.
2. Production journal, identity anchor, catalog/dedup recovery and readiness gates. An empty directory after recovery failure must never mean a fresh tenant.
3. TLS/mTLS principal mapping, protected issuer policy, trusted clock and quotas; QUIC must explicitly expose its UDP port when enabled.
4. Qualified NVMe persistence barriers, io_uring capability tests and controlled WAL/A/B storage roots.
5. Measured HugeTLB, cgroup, affinity, FD and memlock budgets including separate SlotHeader rounding, R11 access sidecars, indexes and kernel overhead.
6. CLI configuration validation, readiness/liveness, graceful drain and genuine completion reaping, backup/restore and tested upgrades.

A kernel version or PLP label alone does not qualify a deployment. HugePage reservation must fit the selected complete memory profile and leave the OS reserve; never run host-tuning commands solely from a capacity example. Broad Docker privileges are not a substitute for verifying required operations.

## Target local IPC and service discovery

The proposed local path is `/run/ciagomem/db.sock` using SOCK_SEQPACKET and authenticated peer credentials. FD exports require a native client adapter and immutable read-only windows. A pure JavaScript client uses framed bytes unless that adapter is installed. No current socket listener or `ciagomem-cli ping` command is provided.

All proposed environment variables and precedence are in [CONFIGURATION.md](CONFIGURATION.md). Published SDK/image version strings, startup latency, operational metrics endpoints and performance guarantees require release evidence before being advertised as available.
