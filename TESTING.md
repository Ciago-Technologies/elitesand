# Debugging, diagnostics and safe retries

> **Single-node service update:** Linux TLS1.3/mTLS reactor hosting and durable client retries are implemented in the fixed-Record profile. See [specification20](MASTER_SPECIFICATION.md#20-implemented-single-node-tls-reactor-and-durable-client-retries) and [source deployment](INSTALL.md#run-the-current-single-node-service). BFBS families, Go/TypeScript SDKs and the distributed runtime remain target work; hardware qualification remains required.

**Current:** typed Rust parser/engine errors, simulator failure injection and validation scripts. **Target:** network status codes, IdentityFile, debug/logger options and structured remote diagnostics in specification 17.6. No current @ciagomem/sdk publication is established here.

## Debug the current checkout

```sh
cargo run --release --offline -p ciagomem-engine --example quickstart
cargo test --release --offline -p ciagomem-core --test r11_regressions -- --nocapture
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
```

The parser exposes byte spans through ParseError.at/end. Report them against the exact original UTF-8 string, not character counts or a normalized query. A successful parse does not imply execution or permission. `WriterActive`, `Reassigned`, `WriterBusy`, `NotYetDurable` and repair events are different paths; do not flatten them into corruption.

## Target SDK logger and diagnostic extension

The future SDK accepts debug plus bounded structured error/warn/debug callbacks. Fields include trace/request ID, operation/status, duration and authorized version/LSN/catalog metadata. Debug must not dump raw frames. Redact tokens/signatures, keys, raw documents/schemas, query literals, tool arguments, full primary keys and identity-file pending bytes. Callback failure and log backpressure cannot block CQEs or change a committed outcome.

V1 wire errors contain bounded text detail; exact remote spans are not encoded automatically. Structured diagnostic_v1 negotiates result_kind6, code, primary/related UTF-8 spans and a bounded message. Validate all spans before exposing them. An SDK may preserve local parser spans without pretending an arbitrary text server error has a structured span.

## Target crash-safe client identity

IdentityFile has an exclusive process lock and a random nonzero client_id. Persist sequence, semantic digest and exact pending operation before sending; persist terminal result before clearing it. Validate checksums and directory-fsync boundaries. Recovery retransmits the identical semantic request with the same sequence; authentication/routing may renew without changing that digest. Never share one writable identity file among processes or silently recreate a corrupt file.

## Target outcomes

| Outcome | Correct response |
| --- | --- |
| PermissionDenied3 | Renew through trusted issuer when appropriate; maintain identity during reconciliation |
| VersionConflict5 | Terminal; read/recompute, then allocate a new intent/sequence |
| ConditionFailed, status6 | Business predicate false; no partial change; changed arguments require a new intent |
| Busy8 | Bounded jittered backoff; retain original pair/digest; a lost response may still need reconciliation |
| IdempotencyConflict9 | Stop; same sequence was used with different semantics |
| RetryTooOld10 | Outcome may be unavailable. Reconcile through business history/IDs; never automatically repeat using a fresh client_id |
| SequenceGap11 | Recover missing pending state; do not skip sequence |
| NeedsRepair12 | Preserve evidence and require verified A/B/WAL recovery; never recreate as empty |
| Indeterminate13 or disconnect | Timeout is not abort proof; retain and retry exact original intent |
| UnsupportedFeature14 | Explicit failure; do not downgrade consistency |
| DedupCapacity15 | Respect admission/quota; alternate identities must not bypass the limit |

The target24-hour result-retention default is not an unlimited retry guarantee. Retain high-water decisions after pruning.300-second approval TTL is unrelated. A new business operation after conflict must follow a reconciled terminal outcome, not merely a transport timeout.

## Repair and observability

No automatic repair service or replica backup restorer ships today. A future operator must verify identity, image epoch, checkpoint and WAL coverage before restoring. NeedsRepair is not always a whole-core fence. Distinguish damaged selected copies from cold absence and preserve original files/logs.

Target metrics include active_sessions_total, eviction/dwell/generation headroom, export bytes, migration pause, route mismatch, WAL/archive pressure and CQE latency. Use bounded node/core/class/result labels and sampled hashed identities, not per-user metric cardinality. Existing local logs are evidence, not a running telemetry endpoint.

## Current MemoryBroker errors and reconciliation

The Rust broker returns Engine Error variants, not target network status numbers. Permission covers invalid role/session/boot/expiry; StaleReference covers invalidated/missing job/source lifetimes; Budget covers task/source/read/output/context caps; Duplicate covers conflicting publication/output; Journal means publication/cleanup did not succeed through the adapter. For a lost worker message, use result with the original lease and controller scope. Repeated identical live-job publish returns its reference without another journal write only while non-stale and within publication TTL. Invalidated or expired publications return StaleReference. result retains historical completion metadata; read_context independently revalidates freshness. forget is explicit retention cleanup and invalidates descendants. After process restart old broker refs are invalid; no task recovery service is supplied.

## Target routing and query-worker pressure diagnostics

Distinguish offered requests, admitted outstanding work, queued-only gateway requests, completed requests/sec, active sessions and physical connections. Export bounded-cardinality ingress/payload/output occupancy, Busy counts, per-core queue age, worker jobs/snapshot bytes/pin age, publication-retired bytes, spill/cache pressure and per-stream index progress. Report routing/worker/I/O/commit time separately from the resident point read.

A stale placement epoch requires authenticated route refresh and retry of the same intent; no gateway may create a new mutation sequence simply to reroute. Busy before acceptance is backpressure; a lost ACK or timeout after acceptance is uncertain and follows the existing reconciliation rules. With worker saturation, defer/shed new jobs and preserve genuine CQE progress. Missing/corrupt committed segment references are repair events, not absent actors. The service and these telemetry endpoints remain target integration.

Distinguish cold-fault park time, device completion, validation, replay and runnable delay. Track analytical contiguous coverage, missing changes and retained WAL bytes per stream/lineage. A later observed LSN is not proof of gap-free processing. A cancelled request retains device-owned buffers until genuine completion/qualified teardown; SQ pressure must not stop CQE progress.

## Durable worker outcomes

AdmissionError::Busy means the Rust worker did not accept the request. Error::Budget may also identify WAL capacity or oversized batches before publication. Error::Journal can be indeterminate after an I/O/anchor failure: stop writes and reconcile by reopening verified storage; never blindly repeat a debit. InvalidImage during recovery requires key/schema/history investigation rather than recreating the store. The protocol3 network adapter maps these outcomes as described below; automatic repair is not provided.

## Network and client retry diagnostics

Check the daemon's READY address, boot_epoch and mode. Native startup failure is an error, not a fallback notice. Verify allowed CPU affinity, io_uring access, OpenSSL3.5 LTS >=3.5.9, private key/data permissions, certificate chain/hostname and allowlisted client DER fingerprint. Match PeerUid/audience7/key-epoch1/boot/client ID in the capability. A valid CA-signed but unmapped certificate is denied.

Protocol3 statuses:3 PermissionDenied,5 CAS/stale reference,6 invalid/ConditionFailed,8 Busy,9 IdempotencyConflict,10 RetryTooOld,11 SequenceGap,12 invalid image,13 Indeterminate,14 Unsupported and15 DedupCapacity. A kind1 receipt is a durable terminal outcome even on failure; Connection persists it before advancing IdentityFile. An error without a receipt leaves pending state. Correct current auth/profile/capacity, reconnect after any incomplete exchange and resume **the same saved intent**. On restart renew boot-scoped auth for the same client ID. A changed expected version/query is a new intent only after the previous one is reconciled.

Keep the identity file and its lock; corruption and competing owners are rejected. Read last_result after a client crash that may have persisted its receipt already. RetryTooOld has a durable high-water marker but no retained result: never automatically repeat the operation with a new identity/sequence. Reconcile application state/operator evidence. Do not log tokens, root keys or raw frame payloads. A TLS framing violation closes only that peer; bounded overload must not be interpreted as a committed failure. Record native hardware skips separately from successful portable/TLS tests.

## TLS reactor and durable client checks

```sh
cargo test --release --offline -p ciagomem-engine --test network --test retries -- --nocapture
cargo test --release --offline -p ciagomem-core reactor::tests -- --nocapture
cargo run --release --offline -p ciagomem-engine --bin ciagomem-server -- --help
```

OpenSSL3.5 LTS >=3.5.9 and its CLI are required; fixtures generate temporary CA/server/client credentials. Tests exercise the actual TLS/framing/reactor via connected Unix streams when this sandbox denies TCP listeners. The real TCP and native io_uring tests report explicit skips for unavailable facilities. Lost-ACK tests commit a request without completing its local identity journal, reopen server and identity, reject old boot auth, renew it and recover the original receipt without a duplicate effect. Other cases cover CAS/terminal failure, malformed/fragmented/multiplexed frames, slow peers, quotas, identity corruption/locking and private issuer output. Socket-pair evidence is not native TCP/NVMe power-cut evidence. The CI provider-install build itself is verified when the pushed workflow runs, not by local provider availability.
