# Security, capabilities and AI governance

**Current:** capability/provenance/export primitives and reference Engine authorization. **Target:** a complete authenticated service, namespace/family policy, production clock, transport and operational key management. [Specification17.4](MASTER_SPECIFICATION.md#174-capability-types-clocks-and-target-codec) defines the target codec and clock behavior. Component tests are not proof that arbitrary host applications integrate them correctly.

## Implemented capability primitives

Macaroons use HMAC-SHA256, constant-time signature comparison and conjunctive caveat evaluation. Every mandatory caveat must pass; attenuation restricts authority. Unknown caveats reject.

| Existing caveat | Actual width |
| --- | --- |
| Tenant / Session / BootEpoch | u128 each |
| ExpiresAt / Audience / Ops / KeyEpoch | u64 each |
| PeerUid | u32 |

RequestCtx receives trusted now_ns and peer/session context from the host. The embedded example's monotonic clock is a local domain, not a Unix timestamp. A client must never select the authorization clock or peer UID. Engine admission checks token expiry/boot, identity and operations; this does not create a TLS listener or namespace catalog.

## Target service scopes and roles

Namespace(u32), ClientId(u64), sorted numeric AllowedFamilies(u16 IDs), and explicit NamespaceAdmin are target additions. The exact v2 TLV framing, type widths, MAC chain, max 32 caveats/16-KiB token, unknown-tag rejection and interoperability gates are in17.4. Family names are resolved server-side; a list of names is not the cryptographic set encoding.

A migration/admin role may register schemas only within its namespace. Application roles receive read/write scope for designated families; analytics roles receive bounded read/scan permissions. No role implicitly grants Policy integrity to stored content. Root-key policy reload must validate atomically and recheck queued requests before commit admission; it cannot revoke completed effects.

Target production expiry uses trusted Unix nanoseconds with a nondecreasing high-water observation. Monotonic durations are separate. Clock rollback cannot extend expiry, caller timestamps cannot advance replay clocks, and BootEpoch invalidates old tokens after restart. Missing qualified time/identity/key policy fails readiness. Namespace/family/client authorization is rechecked before serving deduplicated results too.

## Tenant and actor lifetimes

Validate full tenant and actor incarnations in authoritative identity metadata, snapshots, routes/checkpoints and relationships. Recreating a deleted identity obtains a fresh random incarnation. Never infer authorization from an ID-only route or an arbitrary schema field. Some current structures use tenant-scoped indexes rather than the future namespace/family catalog: do not claim every existing WAL/header/index physically stores the same composite tuple.

## CaMeL provenance and action permits

```text
External < AgentDerived < UserDirect < Policy
```

Public untrusted input defaults to External. Trusted labels require configured issuer validation and content-bound Ed25519 attestation. Derivation propagates minimum integrity, source union and reader-set intersection; it cannot launder content into a signed fact. Actual prompt/tool integrations must carry and enforce these labels. Storing a label alone does not prevent every prompt injection.

Endorsement approvals are scoped to content digest, session and boot; maximum approval TTL is300 seconds with bounded replay tracking. This is separate from capability expiry and the target result ledger's retention. Sensitive actions require a consumed permit tied to the authorized tool/sink/argument digest; a label alone is not tool authority. Never hand untrusted actors an issuer key or unrestricted tool executor.

## Buffers, exports and target transports

R11 keeps raw slot/header atomics private, claims lifecycle ranges, excludes CPU payload access while DMA owns a slot and uses checked single-use pin tokens. Unsafe backend submissions/completions retain explicit lifetime contracts. Abandoned I/O/pins fence/retain capacity; they are not automatically repaired by dropping a Rust value.

SealedExportWindow is an isolated immutable projection, not the writable engine arena. The current Linux primitive uses shrink/grow/future-write/seal seals and an O_RDONLY client descriptor. A target4-KiB response window has a64-byte header and at most4032 data bytes; projection materialization and descriptor transfer cost are separate from getter access. Per-session/global budgets include FD/VMA overhead.

TLS1.3/mTLS and QUIC, SO_PEERCRED socket sessions and native FD SDK adapters are target service integrations. Disable replayable0-RTT mutations. Header CRC detects accidental corruption; transport authentication and schema/root verification remain necessary. Never treat CRC as a MAC.

## Reporting vulnerabilities

This repository does not establish a staffed security mailbox or response SLA. Use a maintainer-provided private channel when available; avoid putting tokens, keys, customer payloads or exploitable private data into public issues. A future release must publish its supported-version and disclosure process before production distribution.

## Worker orchestration boundary

MemoryBroker owns its Engine and restricts worker leases to exact sources/output, tenant/session/audience/boot and deadlines. It accepts summary text as data, derives minimum source integrity and never issues a tool permit. Raw External evidence remains External. It invalidates dependent summaries on controlled source updates and requires host-reported revisions for external files. Rust type privacy is not an OS sandbox. See specification18 and the research/threat assessment in docs/MEMORY_ORCHESTRATION_RESEARCH.md.

## Target gateway, snapshot and segment trust boundaries

A pooled gateway connection authenticates the gateway, while every request retains its own end-client capability, session, scope and idempotency identity. The backend rechecks authorization/expiry/boot and current placement authority; hashing/routing never grants access. Validate response stream ownership so multiplexed tenants cannot receive each other's results. Preserve protected completion/decision capacity when rejecting untrusted ingress.

Query Workers receive bounded immutable snapshots/manifest generations with full identity, provenance and visibility metadata. They receive neither mutable arena pointers nor implicit write authority. Revalidate proposed writes at the owner and current access/deletion policy before returning search results. Query-wide consistency requires an explicit consistent cut; independently pinned actors do not provide it.

Remote segment references are controlled logical IDs, not arbitrary client URLs. Apply tenant quotas, bounded reads/decompression, authenticated storage credentials, digest/format verification and manifest-generation retention. A stale index cannot reveal revoked data or resurrect a tombstone. Left-Right publication retains guarded reclamation and permission checks; untrusted local processes still use sealed exports/framed bytes. These are target integration controls under15.13–15.17, not a completed host sandbox.

Remote hot publications require current serving authority and request visibility, not just a matching placement epoch. Unreachable endpoints cannot promise instant revocation; they must fail closed without valid authorization proof. Named-version reads, read-your-writes and linearizable latest reads are distinct contracts. Publication guards protect lifetime, not authority, and a slot pin does not freeze mutable bytes.
