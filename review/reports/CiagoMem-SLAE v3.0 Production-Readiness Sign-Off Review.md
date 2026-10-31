# CiagoMem-SLAE v3.0 — Production-Readiness and Implementation Sign-Off Review

Reviewed document: "CiagoMem-SLAE Complete Technical Whitepaper and Master Engineering Specification v3.0 Final" (2,648 lines; reference crate `ciagomem-slae-v3`, spec lines 338–2634).
Review harness: `ciagomem-v30-review-harness.zip` (the spec's crate extracted unmodified, plus a 2-line compile fix and 16 adversarial tests).
Toolchain: rustc 1.99.1 stable, Miri (nightly), `aarch64-unknown-linux-gnu` cross-check, Linux 6.1.

---

## 0. Verdict

**NOT SIGNED OFF for production. Signed off as an architecture direction and as a reference kernel for Phase-1 implementation, subject to the P0 list in Section 5.**

The v3.0 document is a good architecture. The code it ships is a correct but small reference kernel of about 2,270 lines. That kernel covers the seqlock, handles, the vote table, labels, the parser and the memfd window. The spec presents it as the engine.

Three things block a production sign-off:

1. **The shipped crate does not compile on stable Rust.** There are two errors:
   - E0106 at `parse_named_num`, a missing lifetime.
   - E0658 at `st_low[..kw_end].as_str()`, which uses an unstable `str_as_str` API.
   
   After a 2-line fix, the 5 bundled tests pass, Miri is clean on the 4 non-FFI tests, and the aarch64 check passes.
2. **The modules that carry the ACID claim do not exist.** Spec line 333 says the crate "restores the full `envelope`, `io`, `lifecycle`, `gclog` … modules". The code contains none of the following:
   - envelope, gclog, recovery, `io_uring` use, reactor, `RouteEntry`, CRC32C, left-right promotion, or the agent request ring.
   
   A grep for each name returns zero hits. `io-uring` is declared in `Cargo.toml` and never imported. ACID durability, crash recovery, group commit and the 2.9× write-amplification figure therefore have no code behind them. The prose itself gives two figures: 2.90× at spec line 61 and ~2.98× in the summary.
3. **The authorization model has a P0 hole.** `validate_query` has no notion of the caller's tenant. Any session can read any actor, and can run `scan actors[*]` without operator scope (test S1). Mutate bodies can name foreign actors (S2). The defects in Section 1.6 compound this: forgeable endorsements, an unsigned caveat chain, incarnations that repeat after restart, and a 64-principal reader set.

| v3.0 headline claim | Status in code | Evidence |
|---|---|---|
| Boehm seqlock with hardware deadline and poison detection | Seqlock is correct (Miri-clean). The deadline uses `Instant` (vDSO `clock_gettime`), not `rdtsc`/`CNTVCT_EL0`. `SlotArena::read` does not call `read_deadline` at all. "Poison" fires on a merely preempted writer. | S9, S10 |
| N×N credit-controlled SPSC mesh | There is no ring code, only a `(VecDeque, usize)` test lane. `ShedAdmission` is advisory: the overflow queue grows without bound. | S11 |
| 64-bit generation handles and keyed PRF registry | Handles are correct. The PRF is a home-grown 2-round ARX, not HMAC-SHA256. Its worst-case avalanche is 24 bits, where about 64 is ideal. Incarnation is `(id<<64) \| process counter` and repeats after a restart. | S5, S8 |
| CQE-gated eviction and TruncationPins | No eviction FSM and no CQE gating in the code. `TruncationPins::decide` is an O(dirty-tenants) scan, measured at 168 µs per call with 70k dirty tenants. | S12 |
| Per-core group-commit log rings (EnvelopeV2, CRC32C, ~2.98× WA) | **Absent.** | grep |
| Calvin multi-actor coordination | Only `VoteTable` exists. It has no sequencer, no deterministic scheduler, no lock manager and no replay. The tag and vote bits sit in different words, which leaves an ABA window (Section 1.4). | inspection |
| Strict literal-aware CiaQL parser and validator | It is still substring-based. Stage bodies are raw `&str`. Named arguments are found with `find()`, so a key inside a string literal overrides the real argument. Bare keywords split predicates. `similarity` drops its vector and metric. The parser and the validator disagree about TAKE followed by EXPORT. | S2, S3, S3b–d |
| memfd sealed export windows | Implemented correctly, and the CVE-2024-57937 self-test works. Capacity is not budgeted (Section 4.1). SCM_RIGHTS handoff is not implemented. | bundled c4 |
| CaMeL labels | The lattice is correct. `Endorsement` is an unauthenticated struct. `readers` is a `u64`, so it holds at most 64 principals for 1M tenants. LLM-output labelling rules are undefined. | S6, S7 |

---

## 1. Inter-Subsystem Coherence and Hot-Path Cost

The reference code has no reactor loop, so coherence can only be judged against the spec's reactor prose. The analysis below asks what each module costs if it is wired in the way the spec describes.

### 1.1 Per-iteration hot-path budget (one reactor core, one 200 ns read target)

| Component | Cost per call as written | Hot path? | Verdict |
|---|---|---|---|
| `SeqWord::read_begin`/`read_retry` | 2 relaxed loads and 2 acquire fences, about 2–5 ns local | yes | OK |
| `HardwareClock::now_ns` | `OnceLock` check plus vDSO `clock_gettime`, about 15–25 ns. **On VMs whose clocksource is not TSC/arch-timer, this falls back to a real syscall, which costs microseconds.** It is called twice per spin iteration. | yes, on contention | Replace with calibrated `rdtsc`/`CNTVCT_EL0`. Check clocksource at boot and refuse to start if it is not TSC or arch_sys_counter. |
| `TruncationPins::on_dirty` | SipHash `HashMap` insert, about 20–40 ns, on the **first** mutation after each checkpoint | yes (write path) | Replace (Section 1.3). |
| `TruncationPins::decide` / `min_needed_lsn` | **O(n) scan. Measured at 168 µs per call with 70k dirty tenants** (1M tenants / 14 cores). | if called each loop: catastrophic | Replace (Section 1.3). |
| `ReadCombiner::enqueue`/`flush` | SipHash map, `Vec` allocation per slot, `R: Clone` per waiter | fallback path | Use a pre-sized slab with an intrusive waiter list and no allocation. Reply by `$ref` or window, not `Clone`. |
| `Label::join` / `gateway_allow` | 3 ALU operations | per datum | The cost is negligible. The **storage** cost is not: 17 B per labelled field, persisted in WAL and frames. Budget it. |
| `admit_to_frame` | branches only | frame compile | OK |
| Export-window publish | **Unspecified.** Each mutation of a tenant with K live sessions needs K seqlocked 4 KiB copies (about 100–300 ns each). | write path | Push the window lazily on the session's next read-ring request, or use a per-tenant shared window with per-session `$ref` capabilities. |
| `VoteTable::vote` | 2–3 atomic RMWs on a shared line, about 20–60 ns cross-core | Calvin path | In thread-per-core the votes already arrive over the SPSC mesh, so the table should be **core-local plain `u64`s with no atomics** (Section 1.4). |

### 1.2 The poison detector is semantically wrong for this process model

The build uses `panic = "abort"`. A writer that panics inside a write section therefore kills the whole process, and the private arena dies with it. In-process "poison" is impossible. Every `Poisoned` verdict from `read_deadline` is a writer that was preempted, page-faulted or delayed by an SMI for more than `poison_after` (50 µs).

Test S10 demonstrates this. A writer sleeps 5 ms inside its section, and the reader returns `Poisoned { odd_seq }`. The writer then commits normally.

Odd sequences persist only if the arena outlives the process. That is exactly the hugetlbfs hot-restart proposal (Section 4.6.2), where poison repair becomes a mandatory attach-time step.

**Fix:** remove `Poisoned` from the in-process read API and rename the result to `WriterStalled` (a metric plus a fallback to the owner's request ring). Make poison an **attach-time** concept: walk all headers on re-attach; any slot with an odd seq is rebuilt from WAL.

### 1.3 TruncationPins: O(1) design that exploits per-core LSN monotonicity

Per-core LSNs are monotonic. The first-dirty LSN of tenant *t* is therefore assigned in increasing order, so **insertion order equals `rec_lsn` order**. A FIFO dirty list keyed by insertion is already sorted:

```rust
/// O(1) on_dirty / on_checkpointed / min_needed_lsn. Zero allocation after boot.
pub struct DirtyList {
    // intrusive doubly linked list over dense per-core tenant slots
    prev: Box<[u32]>, next: Box<[u32]>, rec_lsn: Box<[u64]>, since_ns: Box<[u64]>,
    head: u32, tail: u32, // NIL = u32::MAX
}
impl DirtyList {
    pub fn on_dirty(&mut self, t: u32, lsn: u64, now: u64) {
        if self.rec_lsn[t as usize] != 0 { return; }           // already dirty
        debug_assert!(self.tail == NIL || self.rec_lsn[self.tail as usize] <= lsn);
        self.rec_lsn[t as usize] = lsn; self.since_ns[t as usize] = now;
        self.prev[t as usize] = self.tail; self.next[t as usize] = NIL;
        if self.tail != NIL { self.next[self.tail as usize] = t } else { self.head = t }
        self.tail = t;
    }
    pub fn on_checkpointed(&mut self, t: u32) { /* unlink, rec_lsn[t] = 0 */ }
    pub fn pin_holder(&self) -> Option<u32> { (self.head != NIL).then_some(self.head) }
}
```

This uses 24 B per tenant slot (about 1.7 MiB per core for 70k slots). It belongs in the 524 MiB control budget.

### 1.4 VoteTable ABA window

`vote()` checks `tag == tx_seq` and then does `fetch_or` on a different word, `voted`/`nays`. That leaves a window between the check and the RMW. In that window the decider can `close(tx)` and `open(tx + capacity)` on the same slot. The stale ballot then lands in the new transaction: a stale "yes" can complete a quorum, and a stale "no" can abort an unrelated transaction.

Packing the tag into the vote word would fix the race, but 64 participants already use all 64 bits.

The coherent fix is architectural. In a thread-per-core engine, votes are messages on the SPSC mesh, and the decider is the single consumer. The table becomes core-local `{tag: u64, mask: u64, voted: u64, nays: u64}` with plain loads and stores. Stale ballots are rejected by comparing the `tx_seq` carried **in the message**. This removes the atomics, the ABA window and the cross-core cache-line traffic together.

### 1.5 Credit flow

The `OutboundFlow` overflow queue is unbounded: `ShedAdmission` is returned, but the item is still pushed (S11 reaches a backlog of 100k with `overflow_cap = 8`). Its memory comes from the 512 MiB control heap, so a slow consumer core exhausts the heap. `send` must **refuse** at the cap, and the caller must convert the refusal into admission rejection at the session boundary.

`on_credits` also uses `saturating_add` without a ceiling. A duplicated credit message silently inflates the window. Clamp credits to the lane capacity and count any excess as a protocol violation.

### 1.6 Security-plane coherence (the CaMeL / macaroon / registry interface)

| ID | Defect | Test | Fix |
|---|---|---|---|
| P0-A | `AuthContext` has no `tenant_incarnation`. Reads of foreign actors and fleet-wide scans pass validation. | S1 | Bind `AuthContext { session: VerifiedMacaroon, tenant_inc, scopes }`. Every `Source` must resolve to actors owned by `tenant_inc`; `scan` requires operator scope. |
| P0-B | Stage bodies (`Filter`, `Mutate`, `Project`, `Aggregate`, `JoinActor.target_expr`) are unparsed `&str`. | S2 | Use a real lexer and an expression AST. Make actor references a first-class node so the validator can enforce ownership. |
| P0-C | Incarnation is `(id<<64) \| in-memory counter`, so it repeats after restart. Pre-delete `$ref`s verify after recreate. | S5 | Persist a monotonic incarnation counter in the WAL superblock. Use `inc = HMAC(K, id ‖ counter ‖ boot_epoch)`. |
| P0-D | `Endorsement` is a plain struct, so any caller can mint `author_integrity: Policy`. | S6 | An endorsement must be a MAC'd capability issued only by the user-direct channel (signed by the session key, bound to `sources`). |
| P0-E | `verify_caveats` has no signature chain, and a token without an `Ops` caveat allows every op. | S6b | Implement the macaroon HMAC chain (`sig_i = HMAC(sig_{i-1}, caveat_i)`). Make `Ops` mandatory and default-deny. |
| P0-F | `allowed_prefixes` as documented contains `s3://`, which admits any bucket. | S4 | Use exact bucket plus prefix matching on a parsed URL (scheme, host and path normalised). Reject anything not on the operator-maintained list. |
| P1-G | `readers: u64` means 64 principals. Tenants alias at 1M scale. | S7 | Readers become a small sorted set of principal IDs (inline 2–4, spill to `$ref`), or a per-tenant label namespace with the tenant boundary enforced outside the lattice. |
| P1-H | Home-grown ARX PRF with worst-case avalanche of 24/128 bits. | S8 | Use `hmac` with `sha2`, or `blake3::keyed_hash`, via a `Prf` trait with a known-answer test. Never ship the ARX function behind a feature flag. |
| P1-I | Named-argument extraction via `find()`. String contents override real arguments, a bad value silently defaults to 10, and `top_k` has no bound. | S3 | Fixed by the AST. Cap `top_k` at 1,000 per stage and the result set by bytes. |
| P1-J | LLM-output labelling is undefined. An output computed from an External-labelled context must be External. | — | Rule: a P-LLM output label is the join of every label in its context window, and the Q-LLM output is always External. Encode this in the frame compiler. |

---

## 2. Workspace and Crate Dependency Structure

### 2.1 Finding

**No multi-crate workspace exists in v3.0.** The specification ships one crate, `ciagomem-slae-v3`. The crate names in the review request (`coagomem-core`, `coagomem-wal`, `coagomem-ql`) appear nowhere in the document, and the project name is spelled two ways (coagomem vs ciagomem). Circularity cannot be judged because there is no graph. The single crate already mixes parsing, crypto, storage primitives and Linux FFI in one module tree, which is the coupling a workspace is meant to prevent.

### 2.2 Proposed workspace (strict DAG, enforced in CI)

Fix the name to `ciagomem-*` everywhere.

```
                       ciagomem-types        (no deps: ids, LSN, TenantWord, SlotHandle, ExtentLoc, errors)
                        /    |     \     \
         ciagomem-sync  ciagomem-crypto  ciagomem-label   ciagomem-ql
  (SeqWord, Clock,       (Prf trait,      (Label lattice,  (lexer, AST, parser; no storage,
   SPSC ring, credits)    HMAC/BLAKE3,     endorsement      no crypto, no I/O)
         |                macaroons)       verification)        |
   ciagomem-arena             \             /                    |
   (SlotArena, size classes,   ciagomem-policy  <----------------+
    extent FSM, DirtyList)     (validate_query, frame compiler, gateway)
         |
   ciagomem-io-api   (trait IoDriver: submit/complete; trait Clock; trait Net)  <- no io_uring here
      /        \
 ciagomem-io-uring  ciagomem-sim   (deterministic driver, fault injection, seeded clock)
      \        /
   ciagomem-wal      (EnvelopeV2, CRC32C, gclog, recovery)  depends on io-api only
         |
   ciagomem-txn      (Calvin sequencer, scheduler, core-local vote table)
         |
   ciagomem-reactor  (per-core loop: composes arena + wal + txn + policy + io-api)
      /        \
 ciagomem-export   ciagomem-server (bin: boot checks, seccomp, memfd handoff, metrics)
 (memfd windows,
  SCM_RIGHTS)
```

Invariants to enforce mechanically. Add a CI job that runs `cargo metadata` and fails on any violation.

1. `ciagomem-ql` depends only on `ciagomem-types`. A parser cannot reach storage, crypto or I/O.
2. Nothing except `ciagomem-io-uring` and `ciagomem-server` depends on `io-uring` or uses `libc` I/O syscalls. Everything above `io-api` is generic over `IoDriver`, which lets the same `wal`, `txn` and `reactor` code run under `ciagomem-sim` byte-for-byte.
3. `unsafe` is allowed only in `sync`, `arena`, `io-uring` and `export` (`#![forbid(unsafe_code)]` everywhere else). Track it with `cargo geiger`.
4. `ciagomem-crypto` is the only crate that names a hash or MAC primitive. Everything else goes through `Prf` and `Mac` traits.
5. Feature unification guard: the `sim` feature must never be enabled in the `server` build. Run `cargo tree -e features` in CI.
6. No crate depends on `ciagomem-reactor` except `server` and `sim`-based integration tests.

---

## 3. Verification, Simulation and Chaos Test Plan

The plan below borrows its central idea from FoundationDB. FoundationDB's simulator runs a whole cluster deterministically in a single-threaded process, and its engineers built that framework before the database itself ([FoundationDB Engineering](https://apple.github.io/foundationdb/engineering.html); [FDB SIGMOD paper](https://www.foundationdb.org/files/fdb-paper.pdf)). The plan also follows TigerBeetle's VOPR, which stubs out the clock, network and disk so that any failure replays from a seed plus a Git commit ([TigerBeetle VOPR](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/internals/vopr.md)).

### 3.1 Layered plan

| Layer | Tool and technique | Targets | Exit criterion |
|---|---|---|---|
| L0 Static | clippy pedantic, `#![forbid(unsafe_code)]` outside 4 crates, `cargo geiger`, `cargo deny`, DAG check | all | Zero warnings. Every `unsafe` block carries a `SAFETY:` comment reviewed by 2 engineers. |
| L1 Unit and property | `proptest` | handle pack/unpack, `ExtentLoc`, `Label` lattice laws (associativity, commutativity, idempotence, monotonicity), `DirtyList` against a `BTreeMap` oracle, credit conservation | 10⁶ cases per property in nightly CI |
| L2 UB | Miri, which detects out-of-bounds access, use-after-free, data races and some weak-memory effects ([Miri](https://github.com/rust-lang/miri/)) | `sync`, `arena`, `ql`, `label`, `policy`, `txn` (all non-FFI) | Clean under Stacked and Tree Borrows, with `-Zmiri-many-seeds=0..64` |
| L3a Exhaustive concurrency | `loom`, which permutes executions under the C11 memory model and must use its own atomics behind `cfg(loom)` ([loom](https://docs.rs/loom/latest/loom/)) | SeqWord (1 writer, 2 readers), SPSC ring and doorbell, credit return, export-window publish | Every model completes. Assertions: no torn read accepted, no lost credit, no message reordering. |
| L3b Randomised concurrency | `shuttle` with the PCT scheduler, which scales further than loom but is not sound ([Shuttle](https://github.com/awslabs/shuttle)) | 14-core mesh, read combiner, cross-core Calvin vote delivery | 10⁷ schedules per night |
| L4 Fuzzing | `cargo-fuzz` / libFuzzer ([Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)) | CiaQL lexer and parser (structure-aware with `arbitrary`), envelope decoder, macaroon decoder, FlatBuffer verifier | No panics. Parse→print→parse round-trip identity. Differential check against a reference grammar (tree-sitter or pest). |
| L5 Deterministic simulation | `ciagomem-sim` drives the real `wal`/`txn`/`reactor` crates with a seeded clock, seeded scheduler for 14 virtual cores, and simulated NVMe and network | whole engine | 10⁹ simulated ops per night. Every failure replays from `(seed, commit)`. |
| L6 Crash-point injection | Inside sim: crash after every SQE submit, every CQE, and every fence. Torn writes at 512 B and 4 KiB granularity. Drop or reorder writes that were not flushed. Apply ALICE-style reordering, which matters because applications often depend unknowingly on filesystem persistence ordering ([ALICE, OSDI'14](https://www.usenix.org/conference/osdi14/technical-sessions/presentation/pillai)). | gclog, A/B extent flip, checkpoint, truncation, `$ref` vault | Recovery reaches a prefix-consistent state equal to the oracle for 100% of crash points. |
| L7 Real-hardware power cut | Relay-controlled PDU on 3 NVMe models (with and without PLP) and 2 kernels | `RWF_DSYNC` / FUA semantics, CRC32C detection | 10,000 cuts per model with zero acknowledged-write loss |
| L8 Transaction isolation | Jepsen-style harness with Elle, which infers Adya anomalies (G0–G2) from client histories ([Elle](https://github.com/jepsen-io/elle)) | Calvin multi-actor txns, failover, migration | Strict serializability with zero anomalies across 1,000 runs that include partitions and clock skew |
| L9 Formal | TLA+/PlusCal with TLC model checking | Calvin sequencer, decision and replay; extent eviction FSM, CQE gating and TruncationPins; recovery; shard migration (Section 4.6.1) | Invariants hold at depth ≥ 30 with 3 cores and 4 tenants. Refinement mapping documented. |
| L10 Security | Property tests for IFC (no External to ActiveIntent path without a valid endorsement MAC); seccomp denial tests; mprotect/CVE self-test; tenant-isolation fuzzing (random sessions issuing random queries, oracle = ownership map) | `policy`, `label`, `export`, `server` | Zero cross-tenant reads or writes in 10⁸ fuzzed queries |
| L11 Performance SLO | HDR histograms per core, `perf stat` counters, isolated cores, 24 h soak | read p50/p99/p99.99, commit p99, eviction stall | Gates in Section 3.3 |

### 3.2 DST invariants (checked after every simulated step)

1. `durable_lsn[c]` is monotonic, and no reader with `Visibility::Durable` observes `applied_lsn > durable_lsn`.
2. For every acknowledged commit, the record is recoverable after any crash point.
3. Log truncation never passes `min(rec_lsn)` of any dirty tenant.
4. An extent's A/B flip is published only after the CQE for the new image, and recovery picks the newest image with a valid CRC.
5. The Calvin decision is a deterministic function of `(tx_seq, inputs)`. All participants apply the same decision, and replay after a crash reproduces identical state hashes.
6. Credits are conserved: `credits + in_flight + returned_pending == lane_capacity` per lane.
7. No session ever receives bytes from an actor whose `owner_inc != session.tenant_inc`.
8. Heap usage is ≤ 512 MiB and the arena is ≤ 5,626 pages at all times. Any allocation over budget fails closed, never aborts.

### 3.3 Release gates (bare-metal, 14 isolated cores)

- In-engine resident local read: p99 ≤ 200 ns, p99.99 ≤ 1 µs.
- Agent read through the export window (already published): p99 ≤ 300 ns.
- Single-actor durable commit: p99 ≤ 50 µs on PLP NVMe.
- Cold tenant fault-in: p99 ≤ 250 µs.
- Zero acknowledged-write loss across the L6 and L7 suites. Zero Elle anomalies. Zero cross-tenant oracle violations.

---

## 4. Final Architectural Verdict on the Four Goals

### 4.1 1,000,000 tenants on 16 GiB

The spec's own budget (lines 74–79) sums to exactly 16,384 MiB **with zero slack**.

| Item | Arithmetic | Comment |
|---|---|---|
| Arena | 5,626 × 2 MiB = 11,252 MiB → **11.25 KiB per tenant if all 1M are resident** | The 4 KiB minimum class allows about 2.88M slots in theory. But 16 classes × 14 cores × ≥1 hugepage strands **≥ 448 MiB** before any tenant arrives. |
| Route table | 1M × 32 B = 32 MiB | OK for a flat array. A radix trie (the migration proposal) adds internal nodes that are not budgeted. |
| Export windows | They share the 154 MiB "Log/Export Buffers" line → **at most 39,424 concurrent 4 KiB windows even with zero log buffers**. Each costs 2 fds and 1 VMA. | memfd shmem is charged to the creating memcg (the engine), not `agents.slice`. |
| Kernel | 2,048 MiB including 256 MiB of `struct page` | Leaves about 1.8 GiB for TCP, slab, io_uring rings and page tables. Workable on paper, but untracked. |
| Slack | **0 MiB** | Thread stacks, io_uring registered-buffer pinning and allocator metadata have no line item. Reserve ≥ 5% (about 820 MiB), taken from the arena. |

**Conclusion:** 1M registered tenants with a resident working set of about 10–30% is credible. 1M concurrently resident tenants averaging more than about 10 KiB is not. Concurrent agent sessions cap at a few tens of thousands unless the window budget grows.

### 4.2 Sub-200 ns AI latency

The 200 ns figure is achievable only for an in-engine, resident, uncontended, same-core seqlock read. Agents do not take that path: they read export windows that the engine fills. Their latency is therefore "window read (about 50–150 ns)" plus the staleness the publish policy chooses.

The other paths are slower:

- A cold tenant costs an NVMe read, 10–100 µs.
- A cross-core join costs at least one mesh round trip, roughly 0.5–2 µs with doorbells.
- A Calvin transaction is bounded by epoch batching.

The spec should state latency **per path** (Section 3.3) rather than as one number.

### 4.3 Strict ACID

The design (group commit, A/B extents, Calvin determinism) can deliver it. The v3.0 artifact **cannot demonstrate it**, because the WAL, recovery, sequencer and replay code are absent. Isolation is also undermined today by P0-A and P0-B, since policy does not bound which actors a transaction touches.

**Status: unproven.**

### 4.4 Zero-token waste

This is a marketing claim, not an engineering property. The `$ref` and frame machinery does reduce prompt bytes. The 98.2% figure has no benchmark, corpus or methodology in the document. Replace it with a measured metric: tokens per task against a baseline RAG pipeline, on a published task set.

### 4.5 One-line verdict

The architecture can plausibly scale to 1M registered tenants on 16 GiB, with sub-200 ns in-engine reads and Calvin-strength isolation. **v3.0 does not prove it.** Roughly 70% of the safety-critical surface (WAL, recovery, reactor, I/O, sequencer, eviction FSM) exists only in prose, and the security plane has six P0 defects.

### 4.6 The five appended advanced components

#### 4.6.1 Lock-free dynamic shard migration — ACCEPT, with a stricter protocol (Phase 3)

The described flow (`Migrating`, then source WAL flush and seqlock snapshot, then the destination maps the extent and flips the route) is missing five things:

1. **LSN re-basing.** The destination core's LSN space is different. The migrated tenant needs a `MigrationRecord { src_core, src_lsn, dst_lsn }` in **both** logs so that recovery can stitch them together.
2. **In-flight Calvin txns.** Migration must be sequenced as a Calvin transaction itself: it is ordered in the global `tx_seq`, so every earlier txn applies on the source and every later one on the destination. This also handles readers that hold old handles. The source revokes, and the generation bump makes stale handles fail with `Reassigned`, which the client retries via the route table.
3. **Durable gating.** The route flip may be published only after `dst.durable_lsn ≥ dst_lsn(MigrationRecord)`.
4. **`$ref` and incarnation continuity.** The incarnation does not change, and vault segments are re-homed or remain readable cross-core.
5. **Hot-spot policy hysteresis.** Requiring a minimum dwell time and a load delta above 20% prevents ping-pong migrations.

A "lock-free" route table should mean a seqlocked or RCU route entry. It does not mean lock-free migration.

#### 4.6.2 Zero-downtime rolling upgrade via named hugetlbfs arena — REJECT as specified; ACCEPT a WAL-replay alternative

Named hugetlbfs files are supported by the kernel ([HugeTLB docs](https://www.kernel.org/doc/html/latest/admin-guide/mm/hugetlbpage.html)), but this proposal conflicts with three v3.0 commitments:

1. **Isolation posture.** A named file in a hugetlbfs mount is a filesystem object. It can be opened by anything with the UID or capabilities, and it outlives `PR_SET_DUMPABLE=0`. The private-arena threat model no longer holds.
2. **Layout coupling.** The new binary must be ABI-identical in `SlotHeader`, size classes and FlatBuffer layouts, or it must migrate in place. That converts every release into a memory-format migration.
3. **Correctness after crash.** If the old binary died mid-section, odd seqs, half-written payloads and unflushed WAL tails persist into the new process. The attach path needs a full header walk, poison repair from WAL, and CRC re-verification of every resident image. That costs seconds to minutes for 12 GiB, which erodes the "warm" benefit.

**Alternative:**

- Run the new binary side by side. The old process hands over **fds** via SCM_RIGHTS (WAL, extent files and memfd windows), not the arena.
- The new process warms from the most recent checkpoint plus WAL tail, with a priority prefetch list of the hottest N tenants.
- Route traffic tenant by tenant using the migration protocol above.

This approach also exercises the recovery path on every deploy, which is a feature.

#### 4.6.3 eBPF telemetry — ACCEPT as complementary; REJECT "zero overhead"

Probe cost is not zero. Cloudflare's `ebpf_exporter` benchmark measured about 1,670 ns of overhead per empty uprobe call ([Cloudflare ebpf_exporter benchmark](https://github.com/cloudflare/ebpf_exporter/tree/master/benchmark)). Published measurements also put empty tracepoint and fentry programs at roughly 15 ns and 24 ns, against about 137 ns for a kprobe, with uprobes about two orders of magnitude above tracepoints ([groundcover summary of that benchmark](https://www.groundcover.com/guides/ebpf-monitoring-performance-overhead)).

Policy:

- Use tracepoints and fentry only (`io_uring:*`, `exceptions:page_fault_user`, `tcp:*`).
- **Never place uprobes on the read or commit hot path.**
- Use per-CPU maps and sample page faults.
- Run the loader as a separate privileged sidecar with CAP_BPF and CAP_PERFMON, never the engine UID. This preserves the seccomp profile.

Engine-semantic metrics (pin age, credit stalls, combiner hits) stay as in-process per-core relaxed counters, which cost about 1 ns, are exported by the control thread, and are correlated with eBPF through a shared `core_id` and timestamp.

#### 4.6.4 Lazy FlatBuffer schema upcasting — ACCEPT, with four constraints (Phase 2)

1. The upcast is a **logged mutation**: it writes an `Upcast { from, to, actor }` WAL record before the in-place rewrite. Otherwise recovery replays old-format WAL records onto a new-format image.
2. Upcasters must be pure, deterministic and total. They are tested by property (`upcast(v_n)` then `downcast_view` gives a projection equal to the original fields) and fuzzed with the FlatBuffer verifier on input.
3. Reads must never write. Under the seqlock, readers are wait-free and non-mutating. A read that discovers an old version enqueues the upcast to the owner core and serves an on-the-fly projected view.
4. The A/B extent image retains its `schema_version`. The checkpoint writes the new version into the inactive copy only. The `layout_version` byte in `TenantWord` gates readers. Labels are carried through the upcast unchanged, and they are part of the upcaster's signature.

#### 4.6.5 Active-active multi-region geo-replication with WAN Paxos — ACCEPT region-pinned only; REJECT "active-active without sacrificing local speed" for shared actors

Calvin's design already accommodates WAN sequencing by batching transactions into epochs and replicating the input log ([Calvin](https://cs.yale.edu/homes/thomson/publications/calvin-sigmod12.pdf)). The cost is unavoidable: every cross-region transaction pays at least one WAN Paxos round (about 50–200 ms) before execution.

Coherent design:

- Each actor has a **home region**. Local reads and writes stay sub-microsecond, with region-local sequencing.
- A multi-region transaction goes through a global sequencer tier at epoch granularity and accepts WAN latency.
- Follower regions serve **bounded-staleness** reads from asynchronously replicated logs, never from the global sequence.

True active-active writes to the **same** actor from two regions contradict region pinning. They would require either a WAN round on every write or CRDT semantics, and CRDTs are incompatible with strict ACID on that actor. The spec should choose one per actor class and say so.

---

## 5. Week-1 Implementation Focus (Top 3)

### W1-1 Close the tenant-isolation P0s before writing any storage code

These are P0-A to P0-F from Section 1.6. Deliverables:

- `ciagomem-ql` gains a real lexer, token stream and expression AST, with actor references as AST nodes.
- `ciagomem-policy` enforces `owner_inc == session.tenant_inc` for every referenced actor.
- `ciagomem-crypto` gains HMAC-SHA256 and BLAKE3 behind `Prf`, the macaroon signature chain, a persisted incarnation counter and MAC'd endorsements.
- Exact-bucket destination allowlists.

Exit: tests S1–S7 in the harness are inverted and pass, and a cargo-fuzz parser target runs for 24 h without panics.

### W1-2 Stand up the workspace DAG plus the `IoDriver` trait plus the DST skeleton

Do this before the WAL. Create the crates of Section 2.2 with the CI DAG check. Define `trait IoDriver`, `trait Clock` and `trait Net` in `ciagomem-io-api`. Implement `ciagomem-sim`: a seeded scheduler, a simulated NVMe with torn-write and reorder faults, and a crash-anywhere hook.

Then write `ciagomem-wal` (EnvelopeV2, CRC32C, group commit, recovery) **directly against the sim**, with invariants 1–4 of Section 3.2 running from the first commit. This is the only route to substantiating the ACID claim. It also prevents io_uring from leaking into logic crates.

### W1-3 Fix the hot-path data structures and semantics, and gate them with benchmarks

- Replace `TruncationPins` with the O(1) `DirtyList` (Section 1.3).
- Make `VoteTable` core-local and message-tagged (Section 1.4).
- Remove in-process `Poisoned` in favour of `WriterStalled` (Section 1.2), and wire `read_deadline` into `SlotArena::read`.
- Replace `HardwareClock` with calibrated `rdtsc`/`CNTVCT_EL0`, plus a boot-time clocksource check.
- Make overflow a hard cap with clamped credits.

Add loom models for SeqWord and the SPSC ring, and a `criterion` benchmark suite with the Section 3.3 gates in CI on an isolated-core runner. Exit: S9–S12 inverted, the loom models pass, and the `decide` benchmark shows ≤ 50 ns at 70k dirty tenants.

---

## Appendix A — Harness results

```
cargo test --release
  lib (spec's own tests):    5 passed  (after 2-line compile fix; see src/lib.rs.orig)
  tests/v30_adversarial.rs: 16 passed  (each PASS = defect reproduced)
cargo +nightly miri test --lib -- --skip c4      4 passed, no UB
cargo check --target aarch64-unknown-linux-gnu   OK
Measured: KeyedPrf last-byte avalanche mean 45.5 bits, worst 24 bits (ideal ~64)
Measured: TruncationPins::decide @ 70k dirty tenants = 168 us/call
```

| Test | Defect |
|---|---|
| S1 | cross-tenant read and fleet scan pass policy without operator scope |
| S2 | mutate body names a foreign actor; filter accepts arbitrary text |
| S3 | `top_k` smuggled through a string literal, silent default, no bound |
| S3b | `similarity` loses vector and metric |
| S3c | parser accepts TAKE→EXPORT, validator rejects it |
| S3d | bare `limit` in a predicate is split as a stage |
| S4 | `s3://` prefix admits an attacker bucket |
| S5 | incarnation repeats after restart; stale `$ref` verifies |
| S6 / S6b | forged endorsement elevates integrity; unsigned caveats accepted, no Ops means all ops |
| S7 | 64-bit reader set aliases tenant 1 and tenant 65 |
| S8 | ARX PRF diffusion report |
| S9 | `SlotArena::read` uses the 128-iteration budget, never the deadline reader |
| S10 | poison false positive on a stalled writer |
| S11 | overflow grows past cap |
| S12 | O(n) truncation decision |

To use as regression guards: once a fix lands, flip each assertion; the test then guards against the defect returning.
