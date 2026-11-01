# Debugging, diagnostics and safe retries

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

The Rust broker returns Engine Error variants, not target network status numbers. Permission covers invalid role/session/boot/expiry; StaleReference covers invalidated/missing job/source lifetimes; Budget covers task/source/read/output/context caps; Duplicate covers conflicting publication/output; Journal means publication/cleanup did not succeed through the adapter. For a lost worker message, use result with the original lease and controller scope. Repeated identical live-job publish returns its reference without another journal write; read_context still revalidates freshness. forget is explicit retention cleanup and invalidates descendants. After process restart old broker refs are invalid; no task recovery service is supplied.
