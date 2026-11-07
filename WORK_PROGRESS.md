# Elitesand work progress and completion roadmap

**Status date:** 2026-11-08  
**Repository:** `https://github.com/Ciago-Technologies/elitesand`  
**Current branch:** `feat/core-mem-server-engine`  
**Current release label:** `0.1.1` / Git tag `v0.1.1`  
**Document purpose:** Explain what has been built, what is being worked on next, what gates
must pass, and when Elitesand can honestly be described as fully published and ready for
users.

This is a progress and readiness document. It is not a promise that every target feature in
the specifications already exists. The words **implemented**, **verified**, **qualified**,
and **planned** have different meanings:

| Word        | Meaning                                                                                                               |
| ----------- | --------------------------------------------------------------------------------------------------------------------- |
| Implemented | Code exists in the repository                                                                                         |
| Verified    | A repeatable local or CI check exercised the code                                                                     |
| Qualified   | The required production hardware, workload, failure mode, or operational process was tested and evidence was recorded |
| Planned     | Described in the target specification but not yet delivered                                                           |

## 1. Executive status

### What is working now

- Rust workspace builds offline.
- Core, WAL, query-language, crypto, and engine crates are present.
- Release and all-feature test suites pass locally.
- Formatting, workspace, and documentation validators pass.
- Quickstart and durable restart examples work.
- The alpha benchmark runs with four physical cores on the validated host.
- A GitHub release `0.1.1` exists and `0.1.0` is marked deprecated.
- GHCR image tags `0.1.1`, `v2`, and `latest` were published.
- The feature branch contains the release, whitepaper, progress, and checkpoint
  documentation commits through `2d002c9`.

### What is not complete

- Protected branches have not necessarily received the latest release commits; they require
  reviewed PR promotion.
- The Dockerfile binary path still points at `ciagomem-server`, while the actual Cargo
  binary is `ciagom-server`; the published image must not be used as corrected-image
  evidence until this is fixed.
- Production hardware and capacity qualification is incomplete.
- Power-loss/NVMe durability qualification is incomplete.
- Supply-chain and operational security gates are incomplete.
- Distributed runtime, broad SDK publication, and several target schema families remain
  planned work.

**Overall project state: implemented single-node development/release candidate, not fully
production-qualified and not a complete multi-node product.**

## 2. Completed work

### Phase A — repository and core implementation

**Intent:** Assemble the current single-node Rust implementation and its supporting source,
tests, examples, vendor dependencies, and documentation.

**Completed:**

- Workspace crates:
  - `ciagomem-core`
  - `ciagomem-wal`
  - `ciagomem-ql`
  - `ciagomem-crypto`
  - `ciagomem-engine`
- Vendored dependencies and locked dependency graph.
- Actor/core lifecycle, storage, WAL, query parsing/execution, crypto, authorization, and
  engine integration.
- Runnable examples and regression/adversarial tests.
- Installation, configuration, security, debugging, testing, and specification documents.

**Completion evidence:**

- Source is present in the repository.
- Cargo workspace metadata resolves.
- Existing source and tests are included in the repository history.

**Scope limitation:** This phase is complete only for the current single-node scope. It does
not mean that all target architecture sections, SDKs, distributed ownership, or broad
database features are implemented.

### Phase B — functional verification

**Intent:** Prove that the current code compiles and its checked-in behavior remains stable.

**Completed checks:**

```bash
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
cargo test --release --offline --workspace
cargo test --release --offline --workspace --all-features
```

The all-feature run previously reported **263 passed and 0 failed**. Compiler warnings were
visible; a warning-free baseline was not established.

**Example evidence:**

- Quickstart created actor `1` and read credits value `75`.
- Durable host wrote `100`, mutated to `75`, checkpointed, restarted, and recovered `75`.

**Completion rule:** Re-run the relevant checks after every source, workflow, Dockerfile, or
release change. A previous pass does not automatically validate a later revision.

### Phase C — alpha performance experiment

**Intent:** Test the shared-nothing worker/queue design under a bounded synthetic workload.

**Completed implementation change:**

- `TARGET_CORES` changed from `8` to `4` in
  `ciagomem-engine/examples/alpha_benchmark.rs`.
- The validated host selected CPUs `[0, 2, 4, 6]`, excluding the lower-frequency CPUs.

**Observed four-core run:**

| Measurement      |       Observed value |
| ---------------- | -------------------: |
| Workers          |                    4 |
| Throughput       | 2,556,891.39 ops/sec |
| Queue-wait P99   |    6,899 ns / 6.9 us |
| Contract limit   |  100,000 ns / 100 us |
| Service-time P99 |             2,979 ns |
| Errors           |                    0 |

**Interpretation:** This is a smoke measurement on one host. It is not proof of universal
latency, million-user capacity, production throughput, thermal stability, or network/durable
write performance.

### Phase D — release preparation

**Completed:**

- Release commit `79d3ea1` was created.
- Git tag `v0.1.1` was pushed.
- GitHub release `0.1.1 Stable` was published.
- GitHub release `0.1.0` was marked deprecated rather than deleted.
- Container tags were published:

  ```text
  ghcr.io/ciago-technologies/elitesand:0.1.1
  ghcr.io/ciago-technologies/elitesand:v2
  ghcr.io/ciago-technologies/elitesand:latest
  ```

- `GATE.md`, `checkpoint.md`, `CLAUDE.md`, and the release handoff guide were created.
- `WORK_PROGRESS.md` and the full whitepaper were committed in `2d002c9`.

**Important:** Release publication is not the same as production qualification. The release
notes must continue to disclose unqualified hardware, durability, and scaling claims.

### Phase E — current handoff: packaging correction and sequential promotion

**Status:** Blocked pending the Dockerfile binary-path correction.

The next work is not M1 feature coding. First correct and validate the container on
`feat/core-mem-server-engine`, run the documented checks, and promote the resulting commit
through reviewed PRs in this order:

```text
feat/core-mem-server-engine -> dev -> staging -> prod -> main
```

Do not overwrite the existing `v0.1.1`, `v2`, or `latest` tags. After `main` contains the
fix, publish an immutable patch release (expected `0.1.2`) and record its image digest.
Only then begin Phase F supply-chain/deployment preflight.

## 3. Current branch and promotion state

The intended promotion path is:

```text
feat/core-mem-server-engine -> dev -> staging -> prod -> main
```

GitHub protects `dev`, `staging`, `prod`, and `main`. Direct pushes can be rejected even
when local tests pass. Each promotion requires the repository's pull-request review and
the required `build` status check.

Known promotion PRs:

|  PR | From                          | To        | State at last checkpoint |
| --: | ----------------------------- | --------- | ------------------------ |
|  #8 | `feat/core-mem-server-engine` | `dev`     | Open                     |
|  #7 | `feat/core-mem-server-engine` | `staging` | Open                     |
|  #6 | `feat/core-mem-server-engine` | `prod`    | Open                     |
|  #5 | `feat/core-mem-server-engine` | `main`    | Open                     |

Do not assume all four PRs should be merged in parallel. The safer release process is:

1. Review and merge feature -> `dev`.
2. Validate `dev`, then promote `dev` -> `staging`.
3. Validate `staging`, then promote `staging` -> `prod`.
4. Validate `prod`, then promote `prod` -> `main`.

## 4. Next engineering phases

### Phase E — packaging correctness and release hygiene

**Intent:** Ensure the image actually contains and starts the binary produced by Cargo.

**Work required:**

1. Confirm the binary target in `ciagomem-engine/Cargo.toml` and the built output.
2. Confirm the `COPY` path and `ENTRYPOINT` in `Dockerfile` match that binary exactly.
3. Build the release binary.
4. Build the container.
5. Run the container with `--help` and, where possible, a controlled smoke flow.
6. Commit the corrected Dockerfile on the feature branch.
7. Re-run CI-equivalent checks.
8. Promote the correction through the protected branches.
9. Publish a corrected image with a new immutable patch tag if the existing release tag
   cannot be safely republished.

**Exit criteria:**

- Dockerfile path equals the actual binary path.
- `cargo build --release --offline -p ciagomem-engine --bin ciagom-server` passes.
- `docker build` passes.
- `docker run IMAGE --help` exits successfully.
- Published image digest corresponds to the corrected source commit.
- Release notes identify the image tag and digest.

### Phase F — supply-chain and deployment preflight

**Intent:** Establish that dependencies and target hosts meet the declared prerequisites.

**Work required:**

- Run `cargo tree -d`.
- Run `cargo audit` or `cargo deny check`.
- Verify the `legacy-audit` feature is absent from release output.
- Verify Cargo.lock and vendored dependency pins.
- Validate kernel, io_uring, CPU affinity, HugeTLB, OpenSSL, NVMe, permissions, and
  memory/FD limits on the actual deployment host.
- Add or document a preflight command that fails clearly when a prerequisite is missing.

**Exit criteria:**

- No unreviewed dependency or supply-chain findings remain.
- The deployment host passes all required preflight checks.
- Failures are explicit; the service does not silently fall back to an unsafe mode.
- Evidence includes host identifiers, versions, date, and tested configuration.

### Phase G — repeatable performance and capacity qualification

**Intent:** Replace the alpha smoke number with evidence that supports the intended workload
and deployment claims.

**Work required:**

- Run on the exact production CPU, kernel, storage, and configuration.
- Repeat warm-up and measurement runs.
- Record p50, p95, p99, max, throughput, queue depth, errors, CPU frequency, temperature,
  scheduler load, VM/hypervisor activity, and RSS.
- Test sustained writes and checkpoint cycles.
- Test the intended tenant/residency scale.
- Separate local compute, queue, network, authorization, query, and durable-I/O latency.
- Store raw output and the exact source SHA in a benchmark artifact.

**Exit criteria:**

- Every claimed threshold has a named workload and measurement method.
- Results are repeatable across the required number of runs.
- No benchmark claim depends on an uncontrolled host or hidden warm-up condition.
- Memory and capacity limits are measured, not inferred from logical byte accounting.
- The final report states what is excluded from each latency number.

### Phase H — durability and failure qualification

**Intent:** Demonstrate that acknowledged durable operations survive the supported failure
and storage conditions.

**Work required:**

- Exercise WAL recovery for all supported tail modes.
- Run repeated simulated power-loss tests.
- Verify checkpoint A/B flip atomicity.
- Qualify each supported NVMe model and filesystem combination.
- Verify FLUSH/FUA behavior.
- Run hard power-loss or an approved equivalent test.
- Preserve recovery logs and identify any indeterminate outcome behavior.

**Exit criteria:**

- Required recovery tests pass with documented counts.
- No acknowledged write is lost under the declared durability contract.
- Recovery never silently recreates damaged state as an empty store.
- RPO/RTO and unsupported failure modes are documented.

### Phase I — operational security and production runbooks

**Intent:** Make the system operable by someone other than the original developer.

**Work required:**

- Complete key generation, rotation, certificate renewal, and incident procedures.
- Define backup, restore, upgrade, rollback, and schema migration procedures.
- Define monitoring, alert thresholds, log retention, and redaction rules.
- Verify secrets never enter source, images, logs, benchmark artifacts, or release notes.
- Test installation and recovery from a clean host.

**Required artifacts:**

```text
DURABILITY.md
BENCHMARKS.md
SECURITY_OPS.md
UPGRADE.md
MONITORING.md
```

**Exit criteria:**

- A trained operator can install, monitor, upgrade, roll back, and recover the service.
- Runbooks contain commands, expected results, failure handling, and ownership.
- Monitoring detects the declared queue, WAL, storage, resource, and authentication failures.

### Phase J — user-facing product completion

**Intent:** Deliver the complete feature set promised to users, not only the current
single-node reference engine.

This phase must be planned against the capability matrix rather than assumed complete. Items
that remain target work include, depending on the selected product scope:

- distributed ownership and replication;
- broader BFBS/schema families and general document arrays;
- complete asynchronous scheduling and maintenance;
- published Go/TypeScript/Rust SDKs;
- hosted/network service packaging;
- repair and backup integration;
- workload-specific query, vector, time-series, and analytical capabilities.

**Exit criteria:** Every user-facing feature in the chosen release scope has implemented code,
tests, documentation, compatibility notes, operational support, and a release artifact.
Features outside the scope must be clearly labeled planned rather than implied.

## 5. Gate summary

The detailed gate checklist is [GATE.md](GATE.md). Current status:

| Gate | Area                                 | Status               | Completion evidence required                              |
| ---- | ------------------------------------ | -------------------- | --------------------------------------------------------- |
| 0    | Baseline tests and validators        | Passing locally      | CI run on the exact release commit                        |
| 1    | Dependencies and supply chain        | Partial              | Audit/deny results, duplicate check, legacy-feature check |
| 2    | Hardware and kernel preflight        | Incomplete           | Actual host qualification record                          |
| 3    | Capacity and performance             | Incomplete           | Repeated production-hardware benchmark report             |
| 4    | Durability and power loss            | Incomplete           | Storage/failure test report                               |
| 5    | Cryptographic hygiene and operations | Partial              | Key/certificate/secret operational evidence               |
| 6    | Runbooks and monitoring              | Incomplete           | Required operational documents and clean-host drill       |
| 7    | Distributed runtime                  | Out of current scope | Separate implementation and qualification plan            |

**Rule:** Gate 0 passing does not authorize a production-capacity claim. Gates 1–6 must be
completed for the corresponding production claims. Gate 7 is required only when distributed
features are included in the product promise.

## 6. What “fully published for users” means

Elitesand is fully published for users only when all applicable conditions below are true:

### Source and release

- [ ] The chosen user-facing scope is written down.
- [ ] All source files for that scope are tracked in Git.
- [ ] `main` contains the exact release commit.
- [ ] CI passes on the exact release commit.
- [ ] Version/tag/release notes identify the same source revision.
- [ ] Deprecated versions are labeled without rewriting history.

### Installation and package

- [ ] A clean user can follow `INSTALL.md` successfully.
- [ ] Cargo build or the supported package installation works from a clean environment.
- [ ] Docker image builds from the documented source.
- [ ] Docker image starts with the documented command.
- [ ] Container tags and digest are recorded.
- [ ] Runtime prerequisites and permissions are documented.

### Functional behavior

- [ ] Quickstart works from a clean checkout.
- [ ] Durable restart behavior is verified for the declared durability mode.
- [ ] Supported query, mutation, authorization, and error paths have tests.
- [ ] Unsupported features fail explicitly and are documented.

### Performance and reliability

- [ ] Benchmark methodology and raw evidence are published.
- [ ] p50/p95/p99 and throughput are reported for named workloads.
- [ ] Capacity and memory claims are measured on target hardware.
- [ ] Power-loss and storage behavior meet the declared contract.
- [ ] Monitoring and alert thresholds are deployed or documented.

### Security and operations

- [ ] Dependency and secret scans pass.
- [ ] Key/certificate lifecycle is documented.
- [ ] Backup, restore, upgrade, rollback, and incident runbooks are tested.
- [ ] Logs and metrics avoid secrets and unbounded sensitive cardinality.
- [ ] An operator other than the original author can run the system.

## 7. Definition of done for the current release line

The current `0.1.1` release line is **not fully complete** until:

1. Packaging correctness is fixed and verified.
2. The release commit is promoted through `dev`, `staging`, `prod`, and `main`.
3. The final image is rebuilt from the promoted source and its digest is recorded.
4. Gates 1 and 2 have evidence for the intended deployment environment.
5. Gates 3 and 4 are either completed or the release is explicitly labeled a development/
   reference release with no production capacity or durability claim.
6. Required operational runbooks are present.
7. Release notes and README accurately separate implemented, verified, qualified, and planned
   capabilities.

## 8. Handoff instructions

The next developer or AI agent should:

1. Read this file, `checkpoint.md`, `GATE.md`, `README.md`, and `TESTING.md`.
2. Inspect `git status --short --branch`.
3. Confirm the current branch and remote heads before editing.
4. Continue from the first incomplete phase above.
5. Preserve unrelated worktree changes.
6. Never force-push protected branches or rewrite published tags.
7. Add tests and directly related documentation for code changes.
8. Record new evidence with the exact commit SHA, host, command, date, and limitations.
9. Update this file when a phase changes status.

## 9. Related documents

- [checkpoint.md](checkpoint.md): detailed continuation state and known blockers.
- [GATE.md](GATE.md): production gate checklist.
- [README.md](README.md): current capabilities and non-claims.
- [TESTING.md](TESTING.md): tests and verification methodology.
- [INSTALL.md](INSTALL.md): installation and service prerequisites.
- [SECURITY.md](SECURITY.md): security boundaries.
- [DEBUGGING.md](DEBUGGING.md): diagnostics and retry behavior.
- [Dockerfile](Dockerfile): container packaging.
- [`.github/workflows/rust.yml](.github/workflows/rust.yml): CI checks.
- [ELITESAND_RELEASE_GUIDE.md](ELITESAND_RELEASE_GUIDE.md): branch promotion and release procedure.

## 10. Whitepaper v3.0 alignment

This section maps the progress in this file to
`ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md`.
That whitepaper is the architectural and contractual source for the intended complete
Ciagomem product. Its milestone names are targets, not proof that the corresponding code
already exists.

The whitepaper separates three different things:

1. **Current crate foundation:** code that is present in this checkout.
2. **Implemented single-node service:** the fixed-Record TLS/reactor/retry profile in
   whitepaper Section 20.
3. **Target application and distributed product:** Sections 14–18 and 15.13–15.18,
   which still require milestones, integration, and qualification.

### 10.1 Sections 2–9: core architectural invariants

| Whitepaper area                        | Current status                     | What is present                                                                                                       | What is still required for completion                                                                                                     |
| -------------------------------------- | ---------------------------------- | --------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Storage architecture and memory budget | Foundation implemented             | Slot/lifecycle/WAL-related crates, bounded accounting concepts, extent/checkpoint formats, and ownership checks exist | Qualify actual whole-process RSS, node memory budgets, eviction, repair, and production storage behavior                                  |
| Concurrency and progress               | Foundation implemented             | Seqlock/lifecycle, SPSC mesh/flow, credits, deadlines, pins, and progress regressions exist                           | Complete reactor cancellation/reaping, native DMA/I/O qualification, schedule exploration, and distributed progress guarantees            |
| Identity, slots, routing               | Single-node foundation             | Full handle/incarnation checks, slot ownership, generation checks, and owner-core metadata exist                      | Persistent named attach, authoritative placement, migration fencing, cluster ownership, and failover                                      |
| I/O, checkpoints, and WAL              | Embedded durability implemented    | Group commit, authenticated envelopes, checkpoint/replay, index rebuild, and bounded durable host behavior exist      | Native device/power-cut qualification, backup/restore, archive/object spool, repair, and fully asynchronous reactor integration           |
| Transactions and recovery              | Local/reference paths implemented  | Engine mutation/recovery paths, deletion jobs, links, checkpoint replay, and durable retry ledger exist               | Integrated strict-serializable transaction service, cross-node participant durability, consensus/sequencing, and failure qualification    |
| Authorization, capabilities, exports   | Core and fixed service implemented | Macaroon/capability checks, tenant/session/boot/expiry/peer binding, labels, and bounded exports exist                | Family caveats, key/identity operations, network/operator lifecycle, external rollback-resistant anchor, and production secret operations |
| Information flow and AI frames         | Partial                            | Label/provenance and in-process memory orchestration are present                                                      | Provider/namespace integration, model runner boundary, persisted task recovery, hybrid context acceptance, and cost/quality qualification |

**Completion meaning:** Sections 2–9 are not “fully complete” merely because the data
structures exist. The production exit requires concurrency/failure tests, native hardware
evidence, resource accounting, and operational procedures for every claimed invariant.

### 10.2 Section 10: CiaQL, indexing, relationships, and retrieval

The current engine supports a reference scalar query/mutation profile plus selected
relationship, deletion, text, vector, authorization, and aggregation behavior. The
whitepaper’s broader CiaQL profile is a target and must not be confused with the current
parser.

| Capability from Section 10                          | Current checkout                     | Completion work                                                                                                          |
| --------------------------------------------------- | ------------------------------------ | ------------------------------------------------------------------------------------------------------------------------ |
| Basic scalar query/mutation and parser spans        | Implemented and tested               | Preserve compatibility and add negative/authorization/failure tests                                                      |
| Relationship lookup and deletion/cascade behavior   | Implemented in reference paths       | Add full crash/replay, cross-family, index, and bounded-cleanup qualification                                            |
| Text/SQ8/vector reference operations                | Partial/reference                    | Define supported index freshness/recall/update/delete semantics and benchmark representative data                        |
| General joins, scans, bitmaps, zone maps, analytics | Partial or reference-only            | Implement bounded plans, worker isolation, snapshot semantics, and resource admission                                    |
| Full target application grammar                     | Target only                          | Implement AST/semantic validation, compatibility/rejection behavior, execution, and wire fixtures                        |
| BFBS schema catalog and lazy upcasting              | Not implemented as a runtime catalog | Deliver M1 verifier/catalog/descriptors, version compatibility, durable registration, recovery, and upcast failure tests |
| Typed arrays, overlays, delta WAL, compaction       | Not implemented as the target path   | Deliver M3 typed vector/array operations, bounded overlays, compaction thresholds, replay, and crash tests               |
| Target query/index publication consistency          | Partial                              | Add consistent-cut, lag, rebuild, delete, and concurrent publication evidence                                            |

**Completion criteria for this area:** every advertised CiaQL operator has a specified
grammar, parser/semantic tests, authorization behavior, resource limits, replay behavior,
and documented unsupported cases. A syntax parser or benchmark alone does not complete the
feature.

### 10.3 Section 11: operations, backup, evolution, and host qualification

The whitepaper requires more than a working binary. Before this area is complete:

- define the durability contract for each deployment profile;
- qualify supported filesystems, NVMe models, FLUSH/FUA behavior, and power-loss cases;
- implement or document backup/restore and rollback handling;
- document schema evolution and trusted upcaster rules;
- provide monitoring for WAL pressure, durable LSN, queue age, pins, free slots, repairs,
  index lag, and resource headroom;
- complete clean-host installation and recovery drills.

Current `INSTALL.md`, `SECURITY.md`, `DEBUGGING.md`, `TESTING.md`, `GATE.md`, and this
progress file are documentation foundations, not proof that all operational requirements
are qualified.

### 10.4 Sections 12–13: workspace and production exit criteria

The current workspace meets a meaningful subset of Section 13.1:

- formatting;
- workspace/specification/documentation validation;
- default and all-feature release tests;
- compile-fail API tests;
- bounded mesh progress checks;
- ordinary Clippy execution with warnings visible.

The following Section 13.2 evidence remains open:

| Required evidence                                         | Current status | Required before a production claim                                                    |
| --------------------------------------------------------- | -------------- | ------------------------------------------------------------------------------------- |
| Static unsafe/dependency/license audit and warning policy | Partial        | Scoped unsafe review, `cargo audit`/`cargo deny`, and an explicit warning disposition |
| Property tests                                            | Partial        | Lattice, dirty-list, generation, and credit-conservation campaigns                    |
| Miri/loom/Shuttle/randomized schedules                    | Open           | Run supported subsets and record intentional leak/retention assumptions               |
| Parser/security fuzzing                                   | Open           | Structured grammar fuzzing and tenant/owner authorization oracle                      |
| Deterministic whole-engine simulation                     | Partial        | Seeded WAL/transaction/reactor/network/storage integration                            |
| Crash and real power loss                                 | Partial        | All fence/submit/completion paths plus qualified NVMe cuts                            |
| Transaction isolation/failover                            | Open           | Strict-serializable history and failure evidence                                      |
| Formal protocol refinement                                | Design gate    | Document refinement for transaction, migration, and extent protocols                  |
| Hardware performance                                      | Open           | Isolated-core histograms and sustained bounded-memory evidence                        |

The whitepaper’s performance values are **objectives requiring qualification**, not current
results:

- resident same-core read: p99 <= 200 ns and p99.99 <= 1 us;
- published agent window read: p99 <= 300 ns;
- single-actor durable commit: p99 <= 50 us on qualified PLP NVMe;
- cold actor fault-in: p99 <= 250 us on qualified hardware;
- cross-core/region paths: separately measured.

The alpha benchmark’s queue-wait result is a different synthetic measurement and must not be
reported as proof of these objectives.

### 10.5 Section 14: application schemas, collections, arrays, wire, and SDKs

Section 14 is the largest gap between the current release candidate and the complete
user-facing product:

| Section 14 capability                            | Status now                                                                     | Required milestone                                                                                |
| ------------------------------------------------ | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| Runtime schema ingestion and `SchemaCatalog`     | Fixed generated ActorImage; no runtime reflection catalog                      | M1: bounded BFBS verifier, durable descriptors/family bindings, compatibility and recovery        |
| Named multi-family collections and `FamilyIndex` | No durable named collection directory with cold members                        | M2: family registry, cold/resident enumeration, epoch-consistent scans and rebuild                |
| Typed arrays and delta compaction                | Scalar `Record` plus separate embeddings; no general typed document vectors    | M3: typed vectors, overlay/compaction/promotion, compact WAL replay and indexes                   |
| Protocol framing and durable retries             | Protocol 3 TLS/mTLS fixed-Record service and durable receipt/high-water ledger | M4: native qualification, UDS/target family interoperability, complete fault fixtures             |
| Embedded Rust client                             | Source `engine::client` and `IdentityFile` exist                               | M5a: publish standalone family-aware Rust SDK and qualify interoperability/faults                 |
| Go and TypeScript clients                        | Not implemented/published                                                      | M5b: generated-root verification, lossless integer types, state persistence and fixtures          |
| CRUD, conditional mutations, links, deletes      | Fixed-record/reference paths exist                                             | M1–M4: family-aware schema/array/ledger integration and failure-safe semantics                    |
| Four-step developer workflows                    | Examples and current Rust workflows exist                                      | Complete generated schema/codegen, local/remote SDK workflows, docs, and clean-install acceptance |

**User-facing implication:** users can use the current fixed-Record Rust/single-node
profile only within its documented boundaries. They cannot yet be promised arbitrary BFBS
families, general document arrays, published Go/TypeScript SDKs, or the complete target
protocol.

### 10.6 Sections 15 and 15.13–15.18: scaling and distributed runtime

The whitepaper explicitly labels the following as target distributed work:

- placement directory and authoritative routing epochs;
- gateway multiplexing, admission, quotas, and bounded lanes;
- live migration, copy plus WAL catch-up, source fencing, and ownership barriers;
- replicas, quorum durability, archive/spool, and recovery;
- distributed Calvin/participant coordination and cross-node transaction recovery;
- warm attach, rolling upgrades, and availability limits;
- Hub-and-Spoke query workers and immutable snapshots;
- asynchronous analytical views and hot publication;
- viral-actor read distribution;
- tiered bulk storage and committed segment manifests;
- cold-data faulting with progress guarantees;
- fifteen modality mappings with independent semantic contracts.

The current crate foundation contains useful local primitives—slots, lifecycle, WAL, SPSC
mesh/flow, voting structures, capabilities, and migration/replication design direction—but
the whitepaper says there is no complete distributed server. Therefore:

- one process or four/eight worker threads is not a cluster;
- an SPSC lane is not network RPC or consensus;
- a microbenchmark is not one-million-request qualification;
- a parser or simulator is not delivery of a database modality;
- pooled connections and a hot-set assumption do not prove memory or capacity claims.

**M6 completion criteria:** implement placement/routing epochs, replicas/consensus,
resharding, migration fencing, failure recovery, gateway/worker isolation, tiered manifests,
and the required adversarial tests. Then publish a workload-specific SLO report that
separates client operations, internal fanout, accepted outstanding work, rejections,
throughput, latency percentiles, memory, failure reserve, and durability.

### 10.7 Sections 16–18: ownership remediation, documentation, and memory orchestration

#### Section 16 — CPU/DMA ownership and pin lifetimes

The current implementation and regressions cover important ownership/lifecycle invariants:
exclusive buffer ownership, pins, pending I/O retention, generation checks, CQE handling,
and guarded exports. Remaining production work includes cancellation/reaping, native DMA
qualification, Miri/model checking, ARM execution, real device teardown, and power cuts.

#### Section 17 — complete application/operator contract

The repository has substantial current documentation and the fixed-Record CLI/client
profile. The complete contract still requires:

- published family-aware service/SDK interfaces;
- target configuration and diagnostics service;
- operator identity/key/certificate lifecycle;
- accurate current-vs-target examples;
- upgrade, rollback, backup, monitoring, and incident runbooks.

Documentation is complete only when every command/example is runnable for the stated profile
and target-only material is clearly labeled.

#### Section 18 — sub-agent memory orchestration

The current `MemoryBroker` is implemented as a bounded in-process component with scoped
leases, cited summaries, revision invalidation, taint/provenance handling, and byte/task
budgets. The following remain target work:

- provider/model execution integration;
- namespace and network sandbox integration;
- persisted task state and restart recovery;
- hybrid retrieval/delta arrays/prior memory frames;
- quality, token, latency, and cost qualification under realistic workloads.

“Zero-token waste,” universal model-cost savings, or model-quality claims must not be made
from the provider-free example alone.

### 10.8 Sections 19–20: implemented single-node service boundary

The whitepaper records Sections 19–20 as the delivered fixed-Record single-node profile:

- embedded `DurableJournal` with authenticated application batches;
- checkpoint/replay and authoritative index reconstruction;
- bounded authenticated host and per-tenant admission;
- TLS 1.3/mTLS TCP reactor;
- Protocol 3 framing and fixed-Record CRUD;
- durable client intent file, receipts, and high-water retry ledger;
- CPU affinity and native io_uring requirements by default;
- loopback portable mode for testing only.

This profile is useful and testable, but it is not the complete Section 14/15 product. The
whitepaper still requires qualification of native transport/storage, power cuts, identity
and catalog integration, repair, backup/restore, rollback resistance, and whole-node
resource enforcement.

**Section 20 completion evidence required:**

1. Native Linux/io_uring service startup and readiness.
2. TLS/mTLS peer mapping and capability validation.
3. Partial/coalesced frame and malformed-frame rejection tests.
4. Dropped-ACK/restart retry tests showing no duplicate mutation.
5. Native storage and power-cut fault matrix.
6. Clean-host installation with external keys, certificates, and persistent data.
7. Sustained-load and resource-bound evidence.

## 11. Ordered roadmap from the whitepaper

The whitepaper gives this dependency order. Do not skip ahead merely because a demo can be
written:

```text
Current node journal/recovery qualification
        |
        v
M1: runtime BFBS SchemaCatalog + identity durability + upcasting
        |
        v
M2: family membership, collection directory, scan epochs
        |
        v
M3: typed documents, arrays, bounded deltas, compaction, array WAL
        |
        v
M4: transport/authentication/dedup/interoperability qualification
        |
        v
M5a: published Rust SDK
M5b: Go and TypeScript SDKs
        |
        v
M6: placement, gateway, replicas, migration, distributed recovery, SLO qualification
        |
        v
Full user-facing modality and database-replacement qualification
```

Codec work may proceed independently against fixtures, but it cannot be shipped as durable
functionality before its storage and recovery dependencies are complete. Every milestone
must include:

- migration and rejection behavior for old formats;
- bounded resource accounting;
- authorization and tenant isolation;
- failure injection and restart behavior;
- deterministic replay;
- documentation and compatibility notes;
- evidence on the intended hardware/transport where applicable.

## 12. Whitepaper-based final definition of “fully complete”

Elitesand is fully complete for **the full v3.0 target**, not merely released, only when:

1. The current single-node journal, checkpoint, host, TLS, retry, and storage profile is
   natively qualified for the declared durability contract.
2. M1 runtime schema catalog and identity/upcasting behavior pass their malformed-input,
   compatibility, recovery, and quota tests.
3. M2 family collections, membership, cold/resident directory, scans, deletion, and rebuild
   pass isolation and restart tests.
4. M3 typed arrays, deltas, compaction, checksums, overlays, WAL replay, and crash-boundary
   tests pass.
5. M4 wire, transport, capability, dedup, idempotency, disconnect, and retry conformance
   passes across supported transports.
6. M5a Rust and M5b Go/TypeScript clients are published, lossless, schema-verifying,
   state-persistent, documented, and interoperability-tested.
7. M6 distributed placement, routing epochs, gateway admission, replicas, migration,
   consensus/participant recovery, warm restart, tiering, worker isolation, and failure
   tests pass.
8. Each claimed workload modality has its own correctness, durability, freshness/recall,
   performance, capacity, and operational evidence.
9. Production exit criteria from Section 13 pass: static/dependency analysis, property
   tests, Miri/model exploration, fuzzing, deterministic simulation, crash/power tests,
   transaction isolation, formal/design review, and hardware performance.
10. Operators have tested installation, secrets/certificates, monitoring, backup/restore,
    upgrade/rollback, incident, repair, and deprecation procedures.
11. The public README, SDKs, package metadata, release notes, and examples distinguish
    implemented, verified, qualified, and planned behavior without overstating guarantees.

Until these conditions are met, the honest product label is:

> **Ciagomem fixed-Record single-node reference/service profile, with an implemented
> embedded durable host and TLS/retry integration, released for controlled evaluation;
> full BFBS-family, SDK, distributed, modality, and production-capacity qualification remain
> roadmap work.**
