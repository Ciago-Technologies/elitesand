# MemoryBroker staleness audit remediation

## Finding disposition

1. **Incarnation-change invalidation:** the proposed public exploit is blocked by current Engine::prepare: changed incarnation on an existing ID returns StaleReference before commit/install. A regression verifies rejected ingest preserves the original source and usable child memory. Nevertheless invalidate now matches source actor IDs conservatively so a future trusted replacement event invalidates old-lifetime dependencies and descendants. A private unit test simulates this event without weakening the public ingestion guard. This is defensive hardening, not evidence of a currently reachable replacement exploit.
2. **Identical stale publication retry:** reproduced. After source ingestion or parent forget marked a published task stale, identical publish returned Ok while context reads rejected the same memory. publish now checks task staleness before the cached-return path.
3. **Related expired-publication retry:** also reproduced with summary TTL shorter than the worker lease. A retry returned Ok for an expired summary. The cached-return path now checks publication expiry using the nondecreasing engine clock; rollback cannot resurrect it.

Authorization/lease checks remain first. A valid fresh identical retry still returns the same reference without another journal write. A changed summary on a valid publication remains Duplicate. result() intentionally provides historical completion metadata after invalidation; it is not fresh-context authorization. read_context retains its independent scope/freshness checks.

## Evidence

Pre-fix logs show source-replacement rejection succeeded, stale-retry failures, a failed simulated replacement event and an expired-summary retry failure. Post-fix tests cover direct and transitive invalidation, source-replacement rejection, forgotten parent, publication expiry/clock rollback and existing idempotency/journal failure behavior. Logs are retained under review/staleness.

This revision preserves the tenant/session/issuer trust assumptions and volatile broker state. IdentityFile, durable task replay and the network idempotency ledger remain planned. No model-token savings or production-performance claim follows from these fixes.
