# Prompt for Perplexity Computer — R6 fixes

Extract `ciagomem-r6-fixed-review.zip` and review the current five-crate workspace under `ciagomem-slae/` with the accompanying R6-revised master specification. `New File/` ZIPs and `review/` text/logs are historical input evidence; do not overwrite current source with them. SOURCE_SHA256.json identifies bundled files.

Read docs/R6_REMEDIATION.md and docs/VERIFICATION.md. Recheck R6-N1 through N7 and all earlier R5 findings against actual callers and cross-component composition. Treat component fixes, unsupported operations, trusted metadata assumptions and production integration as separate claims.

Run both release suites, workspace validator and bounded mesh runner. Independently test shared controls across size classes and WAL errors, publication/fence concurrency, derived-label attestation, random identity creation under joint restore, per-tenant replay quotas and expiry, old approvals/provenance with newly issued tokens after boot changes, all-zero versus damaged images, epoch/metadata validation, retained-WAL recovery, incomplete/failed/deleted replay and route publication, and checkpoint-barrier success/failure for continuously written tenants. Include positive valid operations, not only rejection.

Inspect whether recovery interval/ownership/issuer/boot inputs are established by a trusted caller, and whether each claimed service actually exists. A caller-passed numeric interval is not proof of WAL continuity. A barrier that the owner ignores is not backpressure. Check cleanup, memory accounting and failure paths as well as steady state.

Run Miri/model checking/fuzzing/ARM checks only if available and attribute results to the exact tested source. Historical Miri and ARM compile logs are from the preceding R5 revision. Current full-server replay, anchor deployment, physical power cuts, SQL superiority and million-user capacity are still open, not established by passing tests or smoke benchmarks.

For each new/reopened finding report severity, file/line, preconditions, minimal reproducer, ecosystem-compatible correction and acceptance test. Supply a focused code/test patch ZIP and exact specification corrections if needed. Do not declare production readiness without the missing integration and hardware evidence.
