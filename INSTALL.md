# Installation and deployment

> **Single-node service update:** Linux TLS1.3/mTLS reactor hosting and durable client retries are implemented in the fixed-Record profile. See [specification20](MASTER_SPECIFICATION.md#20-implemented-single-node-tls-reactor-and-durable-client-retries) and [source deployment](INSTALL.md#run-the-current-single-node-service). BFBS families, Go/TypeScript SDKs and the distributed runtime remain target work; hardware qualification remains required.

**Current:** Rust libraries/examples, Linux fixed-Record server/issuer CLI and source Rust client. **Target:** published containers/SDKs, BFBS families and distributed services. The target release checklist is in specification17.7; the implemented service profile is in20.

## Build and run locally

Install Rust1.89+ with Cargo and Linux OpenSSL3.5 LTS >=3.5.9 development/runtime libraries (libssl/libcrypto). CI builds the pinned provider when the OS version is older. From `ciagomem-slae/`:

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

## Target gateway and query-worker deployment

The gateway, Query Worker service and multi-node host in specification15.13–15.18 are not supplied binaries or container releases. Deploy them only after the corresponding durability, transport, placement and failure gates pass. Gateways have their own TLS/socket/session budgets; pooled node connections retain end-client authentication. Separate query hosts have separately accounted RAM/CPU/storage, while colocated workers reduce available reservations within the complete storage-node profile.

Use the single-node durable host as the first implementation/deployment milestone. Multi-model compatibility, bulk segment storage and the one-million-outstanding-request cluster objective require separate qualification; no additional startup command or environment flag makes them available today.

Qualification must include node-side backend networking/TLS memory even with external gateways, HugeTLB accounting, reserved cold-fault/CQE progress capacity and recovery of accepted operations after gateway loss. External read-serving copies require independently qualified authorization/freshness; they are not automatically current owners.

## Run the current single-node service

Build the executable and inspect its exact positional arguments:

```sh
cargo build --release --offline -p ciagomem-engine --bin ciagomem-server
./target/release/ciagomem-server --help
```

Provision a server certificate with the correct DNS SAN and serverAuth EKU, client certificate with clientAuth EKU, their private keys and the trusted CA. Both peers must trust the appropriate CA; these are operator-managed credentials. The server additionally requires an allowlisted SHA256 DER client-leaf fingerprint mapped to a peer UID. This mapping and the capability's PeerUid must agree. Keep all keys and the allowlist private. Use a new directory for initial setup; never regenerate storage/authentication keys for an existing store.

```sh
set -eu
umask 077
mkdir -m 700 ciagomem-service
openssl rand 32 > ciagomem-service/auth.key
openssl rand 32 > ciagomem-service/storage.key
# certs/client.pem is your provisioned client certificate.
openssl x509 -in certs/client.pem -outform DER | sha256sum | awk '{print $1 " 42"}' > ciagomem-service/principals
chmod 600 certs/server.key certs/client.key
./target/release/ciagomem-server serve   ciagomem-service/data ciagomem-service/auth.key ciagomem-service/storage.key   certs/server.pem certs/server.key certs/ca.pem ciagomem-service/principals   127.0.0.1:7443
```

Native mode pins a network owner and storage owner to two allowed CPUs, and requires io_uring initialization. A failure prevents READY; there is no silent fallback. For a restricted development host, append `--portable-loopback` explicitly (bind must be loopback). That test mode still requires TLS1.3/mTLS, signed capabilities, WAL persistence and the retry ledger. It does not qualify native storage. Stop with SIGTERM/SIGINT; the listener stops accepting and drains outstanding work before the storage owner exits. Supervise the process and retain all data/key/identity files. Physical device stalls still require an operator/supervisor recovery policy.

Initialize a private durable client identity **before** minting its capability:

```sh
mkdir -m 700 ciagomem-client
cargo run --release --offline -p ciagomem-engine --example network_client -- identity ciagomem-client/identity
```

Record the printed client_id and the server's READY boot_epoch. Set `CLIENT_ID` and `BOOT_EPOCH` to those decimal values; the following issues a one-hour tenant11/session1 token for mapped UID42. Audience7 and key epoch1 are fixed by this service profile. The operator utility requires the authentication root; application workers receive only the token, never that root or the storage key.

```sh
EXPIRES_UNIX_NS=$(python3 -c 'import time; print(time.time_ns() + 3600_000_000_000)')
./target/release/ciagomem-server issue-token   ciagomem-service/auth.key ciagomem-client/token   11 1 42 "$BOOT_EPOCH" "$CLIENT_ID" "$EXPIRES_UNIX_NS"
cargo run --release --offline -p ciagomem-engine --example network_client --   127.0.0.1:7443 localhost certs/client.pem certs/client.key certs/ca.pem   ciagomem-client/token ciagomem-client/identity
```

The example creates a fixed-Record actor and reads its verified FlatBuffer. If an intent is pending, it resumes that intent first. Every subsequent fresh invocation creates a new actor; the library exposes read/replace/CAS mutate/delete/query for application integration. After a server restart, retain the same client identity, obtain a new token for the new boot epoch into a **new token filename**, then reconnect/resume with that file. issue-token refuses to overwrite files. The sample token grants point READ/WRITE only; scans require separately issued tenant-operator authority. This CLI does not expose a public issuer or arbitrary family/admin service.

Receipt retention is24hours, with persistent high-water marks after expiry. Never reset/delete an identity to escape an unresolved write. A terminal failure receipt consumes the sequence; Busy/auth/transport errors without receipts retain pending state. Version tokens are opaque global commit sequences. See [retry diagnostics](DEBUGGING.md#network-and-client-retry-diagnostics).

Current defaults are intentionally bounded:64 network connections,32 streams/connection,128MiB logical network credits;32 global/8 per-tenant storage tickets; one reference storage shard with10,000 rows/64MiB actor budget and64MiB WAL/recovery budget. These are service integration limits, not a16GiB/million-user deployment profile. General CIAGOMEM_* environment settings below remain target-only; this executable uses the explicit CLI. No registry image or published SDK installation is claimed.

For systemd supervision, [deploy/ciagomem.service](deploy/ciagomem.service) runs the native service as an unprivileged dedicated ciagomem user, creates a0700 StateDirectory, restricts filesystem writes and caps memory at1GiB for the **current small reference-service defaults**. Provision that user, install the binary at its stated path and provision service-readable private credentials under /etc/ciagomem before enabling the unit. The unit binds loopback for a separately managed TLS gateway; clients still use mTLS. Adjust affinity/cgroup/service budgets only after workload qualification. This example is not a16GiB capacity result. Retain externally provisioned system TLS libraries and backup keys/data before upgrades; no installer automatically creates users, rotates keys or enables a service.
