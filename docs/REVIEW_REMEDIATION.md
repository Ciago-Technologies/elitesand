# v3.0 review remediation ledger

> Historical first-review ledger. The [R5 ledger](R5_REMEDIATION.md) supersedes identity, image-format, label, endorsement, scope and flow-control contracts described below.

Source material: `New File/CiagoMem-SLAE v3.0 Production-Readiness Sign-Off Review.md` and `New File/ciagomem-v30-review-harness.zip`, supplied alongside this workspace. Original reference source and defect assertions are preserved as text under `review/v30`; they are not compiled as passing security regressions.

## Changes delivered

| Finding                           | Implementation                                                                          | Regression evidence                                            |
| --------------------------------- | --------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| Compile errors in ZIP parser      | Replace raw parser with `coagomem-ql` typed parser; core re-exports it                  | All workspace builds                                           |
| S1 tenant reads/scans             | `policy.rs`: verified tenant, owner resolver, operator and operation scopes             | `s1_cross_tenant_and_scan_denied`                              |
| S2 hidden actor references        | Parser AST and qualified-field validation, policy traverses referenced actors           | `s2_actor_references_are_validated_nodes`                      |
| S3 named argument smuggling       | Parser consumes tokens; malformed/duplicate arguments rejected; `top_k` 1–1,000         | `s3_named_args_are_literal_aware_and_bounded`                  |
| S3b similarity fields             | Typed numeric vector, metric and threshold retained                                     | `s3b_vector_and_metric_retained`                               |
| S3c TAKE/EXPORT mismatch          | One shared structural rule: only final EXPORT may follow TAKE                           | `s3c_take_export_policy_agrees`                                |
| S3d keyword splitting             | Parser consumes expressions before looking for another stage                            | `s3d_keyword_field_is_not_a_stage`                             |
| S4 broad destinations             | Exact canonical bucket/path matching, no traversal or ambiguous encoding                | `s4_exact_bucket_and_path_boundary`                            |
| S5 incarnation replay             | Synced append-only reservation journal, exclusive lock, HMAC-derived incarnations       | `s5_restart_does_not_reuse_incarnations`, torn-tail/lock tests |
| S6 forged endorsements            | Tenant/source/expiry MAC verified before elevation                                      | `s6_endorsements_require_authentication_and_binding`           |
| S6b unsigned caveats, missing Ops | HMAC chain; Tenant/Expiry/UID/Ops/Epoch all mandatory                                   | `s6b_signature_ops_and_mandatory_caveats_required`             |
| S7 principal aliases              | Bounded sorted explicit u128 reader IDs                                                 | `s7_principals_do_not_alias_modulo_64`                         |
| S8 ARX primitive                  | Dedicated RustCrypto HMAC-SHA256 provider, subtle constant-time equality                | Crypto RFC 4231 KAT and provider-domain tests                  |
| S9 unused deadline                | Both arena read paths call deadline readers                                             | `s9_slot_read_uses_a_deadline`                                 |
| S10 false poison                  | Runtime result renamed `WriterStalled`; writer may subsequently commit                  | `s10_stalled_writer_is_not_declared_poisoned`                  |
| S11 overflow/credits              | Hard cap returns unsent item; clamp returned credits and count excess                   | `s11_overflow_hard_cap_and_excess_credits`                     |
| S12 dirty scan                    | Preallocated intrusive FIFO, monotonic-LSN checks, O(1) head operations                 | `s12_dirty_list_checks_head_only_and_unlinks_arbitrary_nodes`  |
| Vote ABA/duplicates               | Mutable owner-local table; old sequence and duplicate votes cannot mutate new decisions | Vote-reuse and 64-participant tests                            |
| LSN regressions                   | Both arenas reject before mutation; both watermarks use fetch_max                       | Checked primitive and P0 regression tests                      |
| Export capacity                   | Shared session/byte reservation released on allocation error and Drop                   | Linux export budget regression and original sealing test       |
| LLM labels                        | Context join; P-output capped at AgentDerived; Q-output External                        | `llm_output_inherits_every_context_label`                      |
| Unsafe legacy defaults            | v2.5 API available only as `legacy` under nondefault `legacy-audit`                     | Default and all-features builds                                |
| Source/prose mismatch             | Specification core listing generated from source; static DAG and link checks            | `scripts/validate_workspace.py`                                |

The original seqlock concurrency test used a 400 ms throughput threshold that failed under scheduler contention despite validating every accepted snapshot. It now uses a synchronized start, finite writer iterations and a minimum number of validated reader snapshots, without treating a CI machine's throughput as a correctness condition.

## Boundaries that remain open

These are production gates, not completed fixes or implied guarantees:

- The full proposed crate split, syscall/unsafe partition, generic IoDriver/Net interfaces, integrated reactor, server, sequencer and decision-gated cross-log recovery.
- Persistent tenant name/ID mappings and lifecycle semantics. The reservation journal is durable but separate from the WAL superblock. Existing registrations must be restored rather than recreated on every restart. Retain reservation history with backups and never rewind it independently.
- Upgrade the P0 extent format to protect all incarnation bits, version the format, reject stale images on recovery and write deletion tombstones before physical extent reuse.
- Calibrated hardware counters, clocksource boot checks, persistent-arena attach repair, allocation-free read combining and request-ring fallback. `MonotonicClock` currently uses Instant.
- Sequenced credit return, full control-message headroom and bounded admission across the reactor. Excess-credit clamping alone cannot detect every duplicate return.
- Authenticated session transport, trusted owner resolver, signing-key custody, rotation/revocation and invoking query/frame/tool policy on every execution path.
- Vault digest generation, authenticated large reader sets, runtime vector bindings, tenant-name aliases, arbitrary/distributed joins and durable multi-reactor query execution. The new reference executor supplies typed reference joins, a bounded equijoin API, link constraints, schema projection/upgrades, search, analytics and work/result budgets; see [ACTOR_NATIVE_QUERY_ENGINE.md](ACTOR_NATIVE_QUERY_ENGINE.md).
- Cross-process publication protocol, authorized projection construction, SCM_RIGHTS transport, unified export budgets and boot-time FD/VMA/host qualification.
- Synchronous standby replication, archive spool, consistent backups, KMS/DEKs, fork-PITR, durable schema/index catalogs and checkpoint migration, shard migration and multi-region protocols.
- Node-wide allocation enforcement, Miri/loom/fuzz/DST, physical power-loss testing, Elle/TLA+, calibrated benchmarks and production security sign-off.

## Validation

Run from the workspace root:

```sh
cargo test --release
cargo test --release --all-features
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
```

`--all-features` includes explicitly labelled legacy exploit reproductions. They are historical compatibility evidence, not successful remediation tests. Clippy may report existing P0 lint debt and upstream-vendored compatibility warnings; this revision does not claim a zero-warning release gate or results for unavailable nightly/cross-target tools.

## Actor-native execution milestone

`ciagomem-engine` adds the executable counterparts and detailed protocols for the five relational capability scopes. See [the ecosystem design](ACTOR_NATIVE_QUERY_ENGINE.md). The journal interface is publication-gated, but its built-in MemoryJournal is non-durable. Actual mesh request/reply transport is covered by integration tests; pinned production owner loops and full WAL replay are still open. The legacy production gates above remain applicable.
