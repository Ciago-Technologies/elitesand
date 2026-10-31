# Ciagomem-SLAE R5 Remediation Verification (R6)

Scope: `ciagomem-r5-fixed-review.zip` (five-crate workspace including `ciagomem-engine`) and the 634-line v3.0 final specification, both uploaded 31 October 2026. Checked against the 39 R5 findings from the previous review, using the same standard: architectural requirement vs. code supplied vs. executable evidence vs. unverified claim. Everything labelled "observed" below was run in this environment (Linux 6.1.155, x86_64, 2 vCPU, Rust 1.99.1 stable plus nightly Miri). Anything not run is marked "inspected only" or "not assessed".

## 1. Executive verdict

Of the remediation revisions reviewed in this series, this is the strongest. Most R5 component defects are fixed in the code, and the ledger and specification now describe the remaining gaps honestly. The project's own verification claims reproduce exactly. Several fixes are correct at component scope but stop at a boundary the specification still describes as covered.

I found **five confirmed second-order defects** (R6-N1 to N5, each reproduced by a test that passes while the defect exists) and **two design consequences** (R6-N6, N7) that turn acknowledged limitations into availability or liveness failures.

The most consequential:

- **R6-N1 (High).** The W1 claim "arena reads reject fenced cores" is true only inside one size-class arena. Each `SlotArena` allocates its own private `CoreControl` array. The specification requires 16 size classes, so a log fence or a durable-LSN publish reaches at most one of the 16 arenas serving that core. `durable_lsn` is also a public atomic field, so callers can bypass the fence check in `publish_durable()`.
- **R6-N2 (High).** F1 is bypassable through the public `Label::derived(content, parents)`. Anyone holding one attested AgentDerived label can mint a label that passes Fact admission for arbitrary bytes the issuer never signed.
- **R6-N3 (Medium).** T2 residual: incarnations are a deterministic PRF of `(uuid, seq, id)`. Restoring the journal and anchor together reissues an incarnation that was already acknowledged to an earlier lifetime. A random per-create nonce removes the dependency at near-zero cost.

Production readiness: still **not production-ready**. The open gates are unchanged in kind: no deployable identity anchor, no repair/replay service, no integrated reactor, no storage or power-loss qualification, and no long fuzz or model-checking campaign. This is consistent with the ledger's own statement. The engineering work needed is now narrow and concrete (Section 5).

## 2. Evidence executed in this review

| Check | Command (from `ciagomem-slae/`) | Observed result |
|---|---|---|
| Default suite | `cargo test --release --offline --workspace` | All pass; matches the 100-test claim |
| All features | `cargo test --release --offline --workspace --all-features` | 135 passed, 0 failed (their 128 plus my 7 R6 probes) |
| Workspace validator | `python3 scripts/validate_workspace.py` | `VERDICT: PASS` |
| Mesh progress runner | `python3 scripts/check_mesh_progress.py` | Cached-tail regression 1,000 bounded runs passed; concurrent grid 25 bounded runs passed |
| Miri (UB) | `cargo +nightly miri test -p ciagomem-core --lib -- seqlock slot dirty mesh::tests::a_new` | 9 passed, no UB reported |
| Miri, concurrent mesh | `... -- mesh::tests::grid` | 1 passed, no UB reported |
| Miri, parser | `cargo +nightly miri test -p ciagomem-ql --lib` | 8 passed, no UB reported |
| aarch64 type check | `cargo check --target aarch64-unknown-linux-gnu --workspace --all-targets` | Exit 0 (compile only, not executed) |
| Clippy | `cargo +nightly clippy` | Not run: clippy component not installed and unavailable offline |
| Layout sizes | R6 guard test | `Label` 160, `SlotHeader` 128, `CoreControl` 384, `FrameValue` 288 bytes; matches the ledger |
| Engine benchmark | `cargo run --release -p ciagomem-engine --example benchmark` (3 runs) | Bitmap 1,654–1,666 ns/query; scalar 5.9–6.9 ms/query; snapshot 37 ns; accounted 10,260,000 B for 10,000 rows |
| R6 probes | `cargo test -p ciagomem-core --test r6_review`; `-p ciagomem-wal --test r6_wal` | 5 `defect_*` tests pass (defects present); 3 `guard_*` tests pass |

Limits: Miri ran single-seed default schedules. It covers UB on the exercised paths, not weak-memory completeness; loom or Shuttle are still needed ([loom](https://docs.rs/loom/latest/loom/), [Shuttle](https://github.com/awslabs/shuttle), [Miri](https://github.com/rust-lang/miri/)). The aarch64 result is a compile-only check. The benchmark is a smoke measurement on a shared 2-vCPU sandbox, as their own document states.

## 3. New findings (evidence-backed)

| ID / severity | Status | Location | Trigger / violated invariant | Sequence | Impact | Smallest correction | Regression test | Sources | Confidence |
|---|---|---|---|---|---|---|---|---|---|
| R6-N1 High | Confirmed defect | `ciagomem-core/src/slot.rs` 226 (`pub durable_lsn`), 331 (`cores: Box<[CoreControl]>`), 440 (per-arena `CoreControl::default()`), 506 (`any(is_fenced)`) | One log per core must fence and publish durability for every arena that core serves | Log for core 0 fences `class0.core(0)` after an EIO. A class-1 arena on core 0 still serves reads. Separately, `class0.core(0).durable_lsn.store(u64::MAX)` succeeds, and `class1.core(0).durable()` stays 0 | Under the spec's 16-class deployment, W1's fence protects about 1/16 of state. Durable-visibility reads on other classes either never become durable or need a separate publish that the fence doesn't gate. Within one arena, a fence on any core blocks reads for all cores (wide blast radius) | Hold `Arc<[CoreControl]>` once per node and pass it to every arena constructor. Make `durable_lsn` and `fenced` private. Expose only `publish_durable`, `fence`, `durable`, `is_fenced`. Check the fence per core (`self.cores[tw.core]`), not across all cores. Re-check the fence after the seqlock read validates | `defect_n1_fence_and_durable_lsn_do_not_span_size_class_arenas` (invert after fix) | [Rust atomics](https://doc.rust-lang.org/std/sync/atomic/index.html) | High (executed) |
| R6-N2 High | Confirmed defect | `ciagomem-core/src/security.rs` 176–180 `Label::derived`; 356–364 `label_pllm_output` keeps `issuer`/`scope` | Non-External content must carry issuer-signed, content-bound provenance (spec line 204) | Get any attested AgentDerived label L. Call `Label::derived(evil, &[L])`. The result keeps L's issuer, scope, expiry and integrity with `digest = H(evil)`. `admit_to_frame(Fact, …, content = evil)` returns Ok | Fact-position laundering of unsigned content. It can't reach UserDirect or Policy without an endorsement, but an endorsement over the laundered digest from a confused issuer would elevate it | Make `derived` `pub(crate)` and require a `DerivationWitness` minted only by the trusted P-LLM runtime. Or clear `issuer` and require fresh `Label::attested` for the derived bytes. Simplest: derived labels get `issuer = None`, so admission demands fresh provenance | `defect_n2_label_derived_launders_arbitrary_content_into_fact` | [CaMeL](https://arxiv.org/html/2503.18813) | High (executed) |
| R6-N3 Medium | Confirmed residual of T2 | `ciagomem-core/src/security/identity.rs` 302–318 (incarnation = `mac128("ciagomem/incarnation/v2", uuid‖seq‖id)`) | An incarnation must never be reissued to a different tenant lifetime | Snapshot journal and anchor together. Delete and recreate (incarnation I₂ acknowledged). Restore both. Delete and recreate again: I₂ is issued again | Stale WAL records, images, routes or tokens bound to I₂ are accepted as current. The ledger correctly says joint restore defeats the anchor, but the deterministic derivation turns that operational event into silent identity reuse | Derive the incarnation from 128 bits of `/dev/urandom` (or `getrandom`) per create and persist it in the record; the journal already stores the full value. Keep the anchor for rollback detection. Uniqueness then no longer depends on the anchor | `defect_n3_joint_journal_and_anchor_rollback_reissues_incarnation` | [ARIES](https://www.cs.cmu.edu/~15849g/readings/mohan92.pdf) (LSN/identity monotonicity) | High (executed) |
| R6-N4 Medium | Confirmed defect | `security.rs` 200–229 `NonceTracker` | Replay state must be bounded without enabling denial of service, and must survive the approval's lifetime | Tracker capacity 1. Approval nonce 1 with `expires_at = u64::MAX` is admitted. Approval nonce 2 at `now = 999` returns `Capacity` | One global map with no TTL clamp: far-future approvals pin entries forever, and one tenant's traffic exhausts capacity for all tenants. State is in memory only, so a replay within the expiry window is possible after restart if session IDs are reused. `retain` is O(n) on every admit | Clamp `expires` to `min(expires_at, now + MAX_APPROVAL_TTL)` and reject approvals above the cap. Shard capacity per tenant. Use a min-heap or time wheel instead of a full `retain`. Bind session IDs to an engine boot epoch so restart can't reuse them | `defect_n4_nonce_tracker_far_future_expiry_exhausts_shared_capacity` | [Macaroons](https://www.ndss-symposium.org/ndss2014/ndss-2014-programme/macaroons-cookies-contextual-caveats-decentralized-authorization-cloud/) | High (executed) |
| R6-N5 Medium | Confirmed hazard, integration-dependent | `ciagomem-wal/src/envelope.rs` 248–259 (`w[3] != epoch`); `gclog.rs` 306 (epoch +1 per open); `lifecycle.rs` 562 (`(None, None)` → `LoadFailed { errno: 0 }`) | An image written in epoch E must stay loadable after the log reopens in epoch E+1 | Write an image with epoch 1. Reopen (log epoch 2). `validate(…, 2)` returns None for both copies, so `LoadFailed { errno: 0 }` is reported, which is documented as "no valid image in either copy" | If the integrator passes the current epoch, every tenant becomes unloadable after a restart. Worse, errno 0 is indistinguishable from "never written", so a caller that treats it as a fresh tenant loses data silently | Validate `image.epoch <= current_epoch` and `image.epoch == trusted_meta.epoch` only when that metadata exists. Separate `Absent` (no magic) from `Invalid` (magic present, identity/epoch/CRC mismatch), and make `Invalid` emit `NeedsRepair` | `defect_n5_image_epoch_exact_match_rejects_previous_open` | [ALICE](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai) | High for the code behaviour; Medium for impact (depends on the absent metadata store) |
| R6-N6 Medium | Design consequence of the L1 fix | `lifecycle.rs` 559–568 (`(Some, None) \| (None, Some)` → `NeedsRepair`) | Crash during an A/B checkpoint write is the normal case double-buffering exists for | Crash while copy B is being written: A is valid, B is torn, so recovery requests repair. A tenant checkpointed exactly once (B never written) also requests repair | Fail-closed is correct when WAL coverage is unknown, but with no repair service every crash during checkpoint I/O, and every tenant with one image, becomes unavailable on the recovery path. That is an availability regression, not a safety fix | Allow single-copy recovery when `copy.checkpoint_lsn + 1 >= wal_start_lsn`, i.e. the retained log provably covers the gap (D1's pins exist to guarantee exactly this). Record "B never written" in extent metadata | Lifecycle test: torn B during flush + retained WAL ⇒ Loaded and replay; truncated WAL ⇒ NeedsRepair | [ARIES](https://www.cs.cmu.edu/~15849g/readings/mohan92.pdf) | Medium (inspected; not reproduced via the lifecycle harness) |
| R6-N7 Low–Medium | Liveness risk | `dirty.rs` 65–71 (`on_checkpointed` returns `StaleCheckpoint` when `latest > image_lsn`) | WAL truncation must make progress for continuously written tenants | A hot tenant whose image always lags its newest mutation by at least 1 LSN keeps the pin at its first dirty LSN indefinitely (guard test: 989 consecutive stale checkpoints, pin fixed at 10) | Log space grows without bound. Only freezing writes for a checkpoint relieves it, which costs latency | On a stale checkpoint, advance `first_lsn[i]` to the first LSN greater than `image_lsn`. That needs a per-tenant second-oldest LSN, or an owner-side checkpoint barrier that captures the image at `latest` | `guard_dirty_hot_tenant_stays_pinned_until_covering_checkpoint` | [ARIES](https://www.cs.cmu.edu/~15849g/readings/mohan92.pdf) (rec_LSN) | High for behaviour; impact depends on workload |

Areas inspected with no new defect found (limits in brackets):

- Mesh drain and PriorityMesh [no weak-memory model check].
- Group-commit fence and late-CQE rejection in `gclog.rs` 490–800 [component scope only; see N1].
- `openat2` export root: flags `RESOLVE_BENEATH|NO_SYMLINKS|NO_MAGICLINKS`, `O_EXCL`, ENOSYS fails closed ([openat2](https://man7.org/linux/man-pages/man2/openat2.2.html)) [no `RESOLVE_NO_XDEV`; mount-based escape requires privilege].
- memfd seal probe: uses `MAP_SHARED` on the write-capable fd and probes both the `mprotect` upgrade and a new writable map, so the probe is meaningful ([memfd_create](https://man7.org/linux/man-pages/man2/memfd_create.2.html), [6.1.149 ChangeLog](https://www.kernel.org/pub/linux/kernel/v6.x/ChangeLog-6.1.149)).
- Parser preflight, iterative `Drop`, bounds and `Call` rejection [derived `Clone`/`Debug`/`PartialEq` are still recursive, as the ledger says].
- `ValidatedQuery` re-check of scope, session and version in `runtime.rs` 447–457, plus the executor's per-row tenant re-filter at 499 [inspected only; `validate_query` still trusts a caller-supplied `owner_of` closure, so the executor's re-filter is the real guard].
- Cumulative acknowledgement in `flow.rs` [inspected only].
- Ed25519 approval message binding [inspected only; no key ID or rotation, as acknowledged].

## 4. Review of the R5 remediation ledger

Classes: **Correct and covered** = component fix present, regression exists, no bypass found. **Partially fixed** = fix present with a remaining gap or a new R6 finding. **Bypassable / incorrectly fixed** = the claimed property fails. **Not assessable** = needs absent components.

| R5 ID | Ledger claim | R6 classification | Basis |
|---|---|---|---|
| M1 mesh stranding | Fixed | Correct and covered | Same refresh-before-clear fix; 1,000 + 25 bounded runs passed; Miri clean on grid |
| D1 rec_LSN | Fixed | Correct and covered; liveness caveat | 32 B/slot latest LSN; conservative pin; see N7 |
| W1 failed acks | Fixed | Partially fixed | Correct inside `gclog`; the fence does not span size-class arenas and `durable_lsn` is public (N1) |
| I1 image identity | Fixed | Partially fixed | v3 header, 128-bit incarnation, tombstone; epoch exact match plus `errno 0` conflation (N5); CRC, not MAC, so offline forgery is possible; no v2 migration (acknowledged) |
| L1 silent fallback | Fail closed | Partially fixed | Silent fallback removed; over-correction turns normal crashes into repair (N6) |
| T1 restart orphans | Fixed | Correct and covered | Journal replays names, IDs, incarnations, deletes |
| T2 rollback reissue | Conditional | Partially fixed | Anchor CAS works with the test anchor; joint restore reissues (N3); no real anchor |
| T3 zero tail | Fixed | Correct and covered | Only bytes beyond the anchored head are truncated |
| F1 forgeable labels | Fixed | Bypassable via `Label::derived` | N2 |
| F2 endorsement replay | Fixed | Partially fixed | Binding correct; tracker DoS and restart replay (N4) |
| F3 side effects | Fixed | Correct and covered | One-use, non-Clone `FrameValue` permit bound to tool/sink/args |
| F4 symmetric key | Fixed | Correct and covered | Separate signer and verifier types; issuer trust is caller-supplied (acknowledged) |
| F5 reader check | Fixed | Correct and covered | `admit_to_frame` checks readers first |
| P1 root prefix | Fixed | Correct and covered | Inspected; trusted-configuration caveat |
| P2 fleet scan | Fixed | Correct and covered | `QueryScope::Tenant`; fleet backup fails closed |
| P3 HydrateFrame | Fail closed | Correct and covered | Rejected outright |
| P4 adapters | Partial | Partially fixed (as claimed) | Local adapter correct; no S3 ExpectedBucketOwner request path ([AWS](https://docs.aws.amazon.com/AmazonS3/latest/userguide/bucket-owner-condition.html)) |
| Q1–Q5 | Fixed | Correct and covered | Bounds, unknown-call rejection, `$ref` MAC literal, iterative preflight and Drop, `IN` case; Miri clean on parser |
| Q6 typestate | Fixed | Correct and covered (component) | Version re-check; caller-supplied `owner_of` is untrusted, but the executor re-filters |
| X1 seal probe | Fixed | Correct and covered | Meaningful `MAP_SHARED` probe; rejected CVE ID removed ([NVD](https://nvd.nist.gov/vuln/detail/CVE-2024-57937)) |
| X2 raw fds | Fixed | Correct and covered | OwnedFd/BorrowedFd, RAII budget |
| A1 incarnation read | Fixed | Correct and covered | Checked inside the validated snapshot |
| A2 CoreControl | Claim corrected | Partially fixed | Correct that it is private heap state, but the privacy is per arena, which produces N1 |
| A3 WordsMut | Fixed | Correct and covered | Crate-private constructor |
| A4 duplicate types | Fixed | Correct and covered | Root re-exports; legacy behind feature |
| O1 credits | Fixed | Correct and covered (component) | Cumulative index; reactor wiring absent |
| V1 vote conflict | Fixed | Correct and covered | Their test; not independently probed |
| S1 equal LSN | Fixed | Correct and covered | `DuplicateLsn` observed in their test |
| L2 CQE asserts | Fixed | Correct and covered | Validation precedes decrement; their test |
| C1 KeyEpoch | Intentional | Design trade-off, accepted | Rotation forces re-auth; document the outage cost |
| C2 replay/audience | Partial | Partially fixed (as claimed) | No channel binding or command idempotency |
| C3 ref oracle | Fixed | Correct and covered; timing not assessed | Uniform Denied; hash-map timing |
| B1 storage qualification | Open | Not assessable | No O_DIRECT/RWF_DSYNC/power-cut work ([pwritev2](https://man7.org/linux/man-pages/man2/pwritev2.2.html), [LWN](https://lwn.net/Articles/752063/)) |
| G1 tests codify defects | Improved | Correct and covered | Historical tests segregated; bounded runner |
| G2 Miri claim | Corrected | Superseded by this review | I ran Miri and an aarch64 check on this revision (Section 2) |

## 5. Memory budget and per-path latency update

Memory (measured sizes, arithmetic only; not RSS):

- Slot headers: 128 B × 1,000,000 = 122 MiB. Image headers: 128 B per checkpoint copy.
- Dirty arrays: 32 B × 1,000,000 = 30.5 MiB per owner partition set (ledger figure, consistent with the code).
- `CoreControl`: 384 B × 14 cores × 16 classes = 84 KiB. Negligible, but it should be one shared array (N1).
- Labels: 160 B plus up to 64 B of readers plus allocator overhead, about 240 B per labelled value. `FrameValue` is 288 B per admitted frame value.
- Engine actor/index accounting: 10,260,000 B / 10,000 rows ≈ 1,026 B per row (observed). At one actor per registered tenant, 1M resident actors would be about 0.96 GiB before journal, snapshots and allocator overhead.
- The previous conclusion stands: about 7.5–8 KiB per tenant if all are resident on 16 GiB. The 1M-registered objective requires a bounded resident set, not all-resident operation.

Latency (observed smoke only):

| Path | Observed | Notes |
|---|---|---|
| Snapshot lookup/pin | 37 ns/call | Not a query |
| Indexed equality + projection, 10k rows | 1.65 µs/query | 7 work units |
| Scalar fallback, 10k rows | 5.9–6.9 ms/query (about 0.6 µs/row) | An unindexed predicate over a 1M-row partition extrapolates to about 0.6 s; budget `query_work` accordingly |
| WAL group commit, checkpoint I/O, recovery | Not measured | Needs real NVMe ([NVMe spec](https://nvmexpress.org/wp-content/uploads/NVM-Express-NVM-Command-Set-Specification-Revision-1.1-2024.08.05-Ratified.pdf)) |

## 6. Prioritized plan

P0 (before any integration build):

1. N1: one shared `Arc<[CoreControl]>` per node, private fields, per-core fence check, post-read fence re-check.
2. N2: remove public `Label::derived`, or make derived labels issuer-less.
3. N5: epoch `<=` semantics plus `Absent`/`Invalid` distinction; `Invalid` emits `NeedsRepair`.

Pre-release:

4. N3: random 128-bit incarnation per create.
5. N4: TTL clamp, per-tenant sharding, time-ordered eviction, boot-epoch-bound session IDs.
6. N6: WAL-coverage-gated single-copy recovery.
7. N7: stale-checkpoint pin advancement or checkpoint barrier.
8. Production `IdentityAnchor` (for example a compare-and-swap object in a separate failure domain) plus a joint-restore runbook.

Production qualification:

- loom or Shuttle on the mesh, seqlock and CoreControl publish/fence.
- cargo-fuzz on the parser, frame admission and journal replay ([cargo-fuzz](https://rust-fuzz.github.io/book/cargo-fuzz.html)).
- Whole-engine deterministic simulation ([FoundationDB](https://apple.github.io/foundationdb/engineering.html), [TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md)).
- Power-cut NVMe campaign; executed aarch64 tests; Elle-style history checks for transactions ([Elle](https://github.com/jepsen-io/elle)).

Optional: MAC (not CRC) over image headers if disk-level tampering is in the threat model; `RESOLVE_NO_XDEV`; key IDs for Ed25519 rotation. None of these require a new crate.

## 7. Exact specification corrections

| Line | Current text (quoted) | Replacement |
|---|---|---|
| 156 | "later CQEs cannot advance published durability, and arena reads reject the fenced core." | "later CQEs cannot advance published durability. Fence and durable state live in one node-wide `CoreControl` array shared by every size-class arena; arena reads on a fenced core reject. (Current code allocates `CoreControl` per arena and exposes `durable_lsn` publicly; this is open item R6-N1.)" |
| 204 | "Higher integrity requires verified **Ed25519 content-bound provenance** from a configured trusted public key." | "Higher integrity requires verified Ed25519 content-bound provenance from a configured trusted public key. Derived labels carry no issuer and require fresh provenance for the derived bytes. (Current public `Label::derived` inherits issuer and scope; open item R6-N2.)" |
| 204 | "Bounded nonce tracking prevents reuse within the session lifetime" | "Per-tenant bounded nonce tracking with a maximum approval TTL prevents reuse within the session lifetime; session IDs are bound to the engine boot epoch." |
| 135 | "A second file restored alongside the journal does not prevent rollback." | "A second file restored alongside the journal does not prevent rollback. Incarnations are drawn from 128 random bits per create, so joint journal and anchor restoration cannot reissue a previously acknowledged incarnation." |
| 137 | "Validation requires the expected incarnation and epoch from trusted metadata and rejects tombstones." | "Validation requires the expected incarnation, rejects images whose epoch exceeds the current log epoch or differs from trusted metadata when present, and rejects tombstones. Images with valid magic that fail validation report `NeedsRepair`, never 'absent'." |
| 154 | "Recovery with only one valid A/B copy also requests repair because retained WAL coverage is not established." | "Recovery with one valid A/B copy proceeds only when retained WAL covers `checkpoint_lsn + 1` onward; otherwise it requests repair." |
| 608 | "Not run for this revision; no nightly toolchain installed in this environment" | "Not run by the authors. An independent reviewer ran Miri (seqlock/slot/mesh/lifecycle/parser subsets, no UB reported) and an aarch64 compile check; loom/Shuttle remain open." |

## 8. Final verification checklist

- [ ] `defect_n1…n5` tests inverted and passing after fixes (harness supplied).
- [ ] One `CoreControl` array per node; `rg "pub durable_lsn|pub fenced"` returns nothing.
- [ ] `rg "pub fn derived"` returns nothing in `security.rs`, or derived labels have `issuer == None`.
- [ ] Incarnation derivation uses a CSPRNG; joint-restore test shows distinct incarnations.
- [ ] Nonce tracker: TTL cap, per-tenant shard, O(log n) eviction, boot-epoch session binding.
- [ ] Image epoch `<=` semantics; `Invalid` ⇒ `NeedsRepair`; errno-0 path only for all-zero extents.
- [ ] Single-copy recovery gated on retained-WAL coverage, with a test for both branches.
- [ ] Hot-tenant pin advancement test.
- [ ] Production `IdentityAnchor` in a separate failure domain; joint-restore drill documented.
- [ ] loom/Shuttle on mesh, seqlock and CoreControl; Miri on the full non-I/O suite; aarch64 tests executed on hardware.
- [ ] Clippy `-D warnings` clean (not run here).
- [ ] Storage qualification (B1): O_DIRECT + RWF_DSYNC on target devices, power-cut campaign.
