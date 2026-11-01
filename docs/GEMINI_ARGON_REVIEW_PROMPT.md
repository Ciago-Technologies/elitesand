# Review prompt

Audit ciagomem-orchestration-review.zip. Start with specification18 and docs/MEMORY_ORCHESTRATION_RESEARCH.md, then review memory.rs, its tests/example and source-to-frame trust boundaries. Check tenant/session/issuer assumptions, source/TTL invalidation, journal failure, retries, quotas and token-economics claims. Run available release tests and validators. Preserve R5–R11 invariants; report reproducible defects with minimal fixes and skipped checks. Distinguish the Rust broker from planned namespace/SDK/durable services.
