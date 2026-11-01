# Application and operations documentation completion

This revision completes the missing specification contracts without changing runtime implementations. Section17 is mirrored in both full specifications; it makes general application CRUD, AI memory, SaaS, telemetry and local services explicit. Wallets and gaming are examples of contention-sensitive business state, not the product's only scope.

## Contracts added

- Consolidated target application grammar for families, arrays, metadata/results, joins, links, transactions, text/vector retrieval and aggregates; source-free conditional mutation commands and semantic limits.
- Version-checked predicate evaluation, terminal-decision idempotency, feature-negotiated LINK RPC and recoverable deletion boundaries.
- Current capability widths versus proposed namespace/family/client scope, canonical target TLV/MAC codec, production expiry-clock bridge and atomic key policy.
- Configuration precedence, exact units/defaults/validation, readiness, immutable settings and complete memory/sidecar/session/export budgets.
- Logging/redaction, negotiated remote spans, crash-safe client identity and safe handling of Busy, Indeterminate, RetryTooOld and version conflicts.
- Release/container/CLI/SDK prerequisites and concrete future acceptance suites and measurement requirements.

## Guides corrected

README, INSTALL, ABOUT, EXAMPLES, USECASE, CIAQL_REFERENCE, SECURITY, CONFIGURATION, DEBUGGING and TESTING now identify runnable current entrypoints and target features. Examples preserve custom schemas and multi-language goals; hypothetical packages/images are not advertised as published. No generic SQL compatibility, unconditional prompt-injection prevention, million-active-user capacity or measured latency guarantee is claimed.

The current two specifications had drifted in formatting; the repository master was used as the canonical base and the full standalone mirror was synchronized. Existing audited sections/source listings and code remain intact apart from the explicit MUTATE cross-reference to17.3. The historical R11 CI file remains verbatim under review/r11/before with its original recorded digest; CI now also runs validate_documentation.py.

## Evidence and cleanup

The current embedded quickstart succeeded. Fresh workspace release tests passed165 default and193 all-feature cases; three real-ring bodies skipped locally. Workspace/specification and documentation validators, formatting and whitespace checks passed. Raw logs are retained under review/documentation/local.

All obsolete outgoing R10/R11/self-audit ZIPs were removed. Unique self-audit evidence was extracted and checksum-verified; original R10 incoming patch/report entries matched their preserved audit digests before removal. The stale parent README was preserved before deletion; the root self-audit report matched the existing archived copy. The empty input directory and regenerable target/cache directories were removed. Source, vendor, Git and historical review evidence remain. The cleanup manifest records fingerprints and evidence locations.

The documentation revision used ciagomem-documentation-review.zip as its outgoing share bundle; the later orchestration revision supersedes it with ciagomem-orchestration-review.zip. It includes the complete standalone whitepaper at archive root, the same content as MASTER_SPECIFICATION.md, all guides, runtime sources, vendor and review evidence. It excludes Git, build caches and secrets outside the repository. No code fixes or production host/SDK implementation are implied by these documentation changes.
