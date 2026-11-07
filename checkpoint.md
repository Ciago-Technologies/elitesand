# Elitesand continuation checkpoint

**Checkpoint date:** 2026-11-08  
**Repository:** `https://github.com/Ciago-Technologies/elitesand`  
**Default branch:** `main`  
**Purpose:** Give the owner, a teammate, or another coding agent enough verified context to continue safely.

## Project phase map

This is the most important section for continuation. Work is organized into phases. A phase
is complete only when its exit criteria are satisfied; passing unit tests alone does not
complete a release or production-readiness phase.

| Phase                                          | Status                                     | What was achieved                                                                                                | What remains                                                                                                       |
| ---------------------------------------------- | ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| 1. Core implementation and repository assembly | Complete for the current single-node scope | Rust workspace, core/WAL/QL/crypto/engine crates, tests, examples, documentation, and vendor sources are present | Future distributed runtime, SDKs, and broader schema families remain out of scope                                  |
| 2. Functional verification                     | Complete locally                           | Release tests, all-feature tests, formatting, workspace/docs validators, quickstart, and durable recovery passed | Re-run after every code or packaging change                                                                        |
| 3. Alpha performance experiment                | Complete as a smoke experiment             | Four-core affinity run passed the 100 us queue-wait P99 target on this host                                      | Repeat on controlled hardware; collect distributions, thermal data, RSS, sustained throughput, and host conditions |
| 4. Release packaging                           | **Blocked**                                | `v0.1.1`, GitHub release, and GHCR tags were created                                                             | Correct the Docker binary path, rebuild, smoke-test, and decide whether a new patch release is required            |
| 5. Branch promotion                            | **In progress**                            | Feature branch and tag were pushed; promotion PRs and CI runs exist                                              | Merge in order: feature -> dev -> staging -> prod -> main                                                          |
| 6. Production qualification                    | Not started / incomplete                   | Gate checklist and limitations are documented                                                                    | Complete Gates 1–6 evidence before making production-capacity or durability claims                                 |
| 7. Future feature development                  | M1 foundation identified; implementation pending | Whitepaper v3.0 capability matrix and M1–M6 dependency order are documented                                  | Complete M1 and its evidence before beginning M2–M6                                                           |

### M1 status before moving to M2

M1 is the first whitepaper implementation milestone:
**runtime BFBS `SchemaCatalog`, identity durability, and lazy upcasting**.
The repository currently has the foundation needed to begin M1, but M1 itself is not yet
complete. Do not mark M1 complete or start M2 based only on the existing fixed-Record
service.

#### M1 work already completed or available as foundation

- Fixed-Record `ActorImage` scalar/text/vector behavior exists and is covered by the current
  engine/reference tests.
- Actor identity/incarnation, generation, slot, ownership, WAL, checkpoint, replay, and
  authoritative index-rebuild primitives exist for the current single-node profile.
- Authenticated durability, capability/tenant checks, deletion handling, and durable retry
  receipts exist in the current service profile.
- Protocol 3 framing and fixed-Record Rust client/retry behavior are implemented as the
  starting compatibility boundary.
- The whitepaper-to-code dependency order, conservative status labels, and M1 exit criteria
  are documented in `WORK_PROGRESS.md`.

These are prerequisites and reusable components; they are not proof that runtime schema
catalog functionality has been delivered.

#### M1 work still required before M2

- Implement a bounded runtime BFBS schema verifier and reject malformed, oversized, or
  unsupported schemas explicitly.
- Implement durable `SchemaCatalog` descriptors, family bindings, schema identity, version
  metadata, and recovery/replay.
- Bind schema/family identity to tenant authorization and the existing identity/incarnation
  model without weakening fixed-Record authorization.
- Define and implement supported schema compatibility and lazy-upcasting rules, including
  deterministic failures for missing, invalid, or incompatible upcasters.
- Integrate catalog state with WAL/checkpoint/replay and ensure corrupt or incomplete schema
  history produces an explicit repair/error state, never an empty replacement.
- Add registration, lookup, read, update, restart, replay, quota, authorization,
  malformed-input, corruption, compatibility, and upcast tests.
- Add wire/storage compatibility fixtures and document versioning, migration, rejection,
  resource limits, and rollback behavior.
- Record repeatable evidence for correctness, bounded memory/CPU, isolation, and recovery.

#### M1 exit gate

M1 is complete only when a supported runtime schema can be registered, durably recovered,
authorized, validated, read, and lazily upcast through the supported version range.
Malformed, unauthorized, oversized, incompatible, and corrupt inputs must fail explicitly.
The implementation, tests, compatibility fixtures, documentation, and evidence must all be
present in the feature branch and pass CI.

Only after this gate passes may M2 begin:
**family membership, collection directory, cold/resident enumeration, and scan epochs**.

### The next coding phase

The immediate coding task is **packaging correctness**, not a whitepaper feature:

1. Change the Dockerfile from `ciagomem-server` to the actual Cargo binary
   `ciagom-server`.
2. Build the release binary from the corrected source.
3. Build and run the image with `--help`.
4. Commit the fix on the feature branch.
5. Re-run the repository checks.
6. Promote that commit through the protected branches.
7. Publish a corrected image. Because `v0.1.1` already exists, do not move the tag casually;
   use `0.1.2` if repository policy does not allow republishing the existing release tag.

After packaging and promotion are complete, follow the `WORK_PROGRESS.md` phases in order:

1. **Phase F / Gate 1–2:** supply-chain audit and deployment-host preflight.
2. **Phase G / Gate 3:** repeatable performance and capacity qualification.
3. **Phase H / Gate 4:** durability, power-loss, and storage qualification.
4. **Phase I / Gates 5–6:** operational security and production runbooks.

Only after the relevant gate evidence exists should the team start claiming production
readiness. These qualification phases are also the whitepaper prerequisite named
**current node journal/recovery qualification**.

### First whitepaper implementation phase after qualification

The first actual coding milestone from
`ciagomem_slae_complete_technical_whitepaper_and_master_engineering_specification_v3_0_final.md`
is **M1: runtime BFBS `SchemaCatalog`, identity durability, and lazy upcasting**.

Do not start M2, M3, M4, SDK publication, or distributed M6 work as the primary phase before
M1 is complete. M1 must build on the qualified journal/recovery path and must include:

- bounded BFBS verification and rejection of malformed or oversized schemas;
- durable schema descriptors and family bindings;
- schema/version compatibility and explicit unsupported-version errors;
- identity/incarnation persistence and recovery;
- lazy upcasting with deterministic failure behavior;
- tenant authorization and bounded resource accounting;
- restart, replay, quota, compatibility, and corruption tests;
- documentation of the wire/storage compatibility contract.

**M1 exit criteria:** a runtime schema can be registered, durably recovered, authorized,
validated, read, and upcast through the supported version range; malformed, unauthorized,
oversized, incompatible, and corrupt inputs fail explicitly; the required tests and evidence
are recorded. Passing existing fixed-Record tests alone does not complete M1.

After M1, continue in the whitepaper dependency order:

```text
M1 SchemaCatalog + identity/upcasting
  -> M2 family membership, collection directory, scan epochs
  -> M3 typed documents, arrays, deltas, compaction, array WAL
  -> M4 transport/authentication/dedup/interoperability
  -> M5a published Rust SDK and M5b Go/TypeScript SDKs
  -> M6 placement, gateway, replicas, migration, distributed recovery, SLO qualification
```

The full product is not complete until the selected user-facing scope also has tests,
documentation, operational support, release artifacts, and workload-specific qualification.

### Phase completion rule

For every future phase, record four things before marking it complete:

1. **Intent:** what user or system problem the phase solves.
2. **Implementation:** which files, commands, and configuration changed.
3. **Evidence:** test output, benchmark output, CI links, package digest, or hardware record.
4. **Exit criteria:** measurable conditions that must pass before moving to the next phase.

If any one of these is missing, mark the phase **in progress** or **blocked**, not complete.

## Current handoff actions

**Current status: NOT FINISHED.** The next person should not start new feature work or
publish another release yet. Complete these steps in order:

1. **Fix the Dockerfile binary name.** The Cargo binary is `ciagom-server`, but the
   Dockerfile currently refers to `ciagomem-server`.

   ```bash
   cd /path/to/elitesand
   sed -i 's/ciagomem-server/ciagom-server/g' Dockerfile
   cargo build --release --offline -p ciagomem-engine --bin ciagom-server
   docker build -t ghcr.io/ciago-technologies/elitesand:0.1.1 .
   docker run --rm ghcr.io/ciago-technologies/elitesand:0.1.1 --help
   ```

   Inspect the diff before committing. Do not use the old image as release evidence.

2. **Commit and push the Docker fix to the feature branch.**

   ```bash
   git switch feat/core-mem-server-engine
   git add Dockerfile
   git commit -m "fix: use the actual server binary in Docker image"
   git push origin feat/core-mem-server-engine
   ```

3. **Merge promotion PRs in order.** Do not merge the currently parallel PRs out of order.
   The required order is:

   ```text
   feat/core-mem-server-engine -> dev -> staging -> prod -> main
   ```

   Existing PRs are listed in Section 3. After the feature branch changes, refresh their
   checks. If the parallel PRs no longer represent the desired order, close them and create
   sequential PRs using the commands in Section 4.

4. **After `main` contains the Docker fix, rebuild and republish the image.**

   ```bash
   docker build \
     -t ghcr.io/ciago-technologies/elitesand:0.1.1 \
     -t ghcr.io/ciago-technologies/elitesand:v2 \
     -t ghcr.io/ciago-technologies/elitesand:latest \
     .
   docker push ghcr.io/ciago-technologies/elitesand:0.1.1
   docker push ghcr.io/ciago-technologies/elitesand:v2
   docker push ghcr.io/ciago-technologies/elitesand:latest
   ```

5. **Verify completion.** The task is complete only when:

   ```bash
   git fetch --prune --tags origin
   git log -1 --oneline origin/main
   gh pr list --repo Ciago-Technologies/elitesand --state open
   gh release view v0.1.1 --repo Ciago-Technologies/elitesand
   ```

   All promotion PRs should be merged, `origin/main` should contain the intended release
   changes, and the Dockerfile/image check should pass.

### Who does what

| Person                | Responsibility                                                                                                                          |
| --------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Owner/release manager | Fix or approve the Docker packaging issue, approve promotion PRs, publish/re-publish the image, and confirm the final release checklist |
| Contributor           | Work only on a feature branch, run local checks, open a PR to `dev`, and respond to review/CI failures                                  |
| Reviewer              | Review the diff, confirm tests and gate evidence, and approve the relevant promotion PR                                                 |
| Future AI agent       | Read this section first, inspect `git status`, never force-push or rewrite tags, and continue only from the first unchecked action      |

### Current blockers

- The release image was built/published before the Dockerfile mismatch was identified; it
  must be rebuilt after correction.
- `dev`, `staging`, `prod`, and `main` are protected and must be promoted through reviewed
  PRs.
- `v0.1.1` already exists; do not move or rewrite that tag. If the corrected image requires
  a new source commit, keep the same release tag only if policy explicitly permits it;
  otherwise use a new patch release such as `0.1.2`.
- Gate 3 (production performance/capacity) and Gate 4 (power-loss durability) are not
  complete. Do not claim production qualification.

## 1. Current state at a glance

The repository is a Rust workspace containing the Ciagomem/Elitesand core, WAL, query
language, crypto, and engine crates. The release work for `0.1.1` has been prepared and
published as a feature-branch commit, Git tag, GitHub release, and GHCR image. The
protected promotion branches have not yet been updated because GitHub requires pull
requests and the required `build` status check.

Do **not** assume that `main`, `prod`, `staging`, or `dev` already contain the release
commit. At checkpoint creation time, the promotion PRs are open.

## 2. Work completed

### Source and documentation changes

The release commit is:

```text
79d3ea1 release: package stable 0.1.1
```

It contains:

- `ciagomem-engine/examples/alpha_benchmark.rs`
  - Changed `TARGET_CORES` from `8` to `4`.
  - On the validated host, physical-core discovery selects `[0, 2, 4, 6]`.
  - The four-core run produced:
    - throughput: `2,556,891.39 ops/sec`
    - queue-wait P99: `6,899 ns` (`6.9 us`)
    - service-time P99: `2,979 ns`
    - errors: `0`
- `Dockerfile`
  - Was added as container packaging, but its binary path still needs correction.
  - Currently copies `target/release/ciagomem-server`; the Cargo target is
    `ciagom-server`.
  - Runs as the non-root `ciagomem` user.
- `GATE.md`
  - Documents production gates and explicitly separates passing local checks from
    unverified hardware, durability, and performance qualification.

An additional local commit added:

```text
08d2350 chore: add gstack skill routing rules to CLAUDE.md
```

`CLAUDE.md` is local repository documentation for agent skill routing. It was committed
after the release commit and is currently ahead of `origin/main` locally. Confirm whether
it should be promoted with the release PRs before merging.

### Validation completed

The following local checks passed during this work:

```bash
cargo fmt --all -- --check
python3 scripts/validate_workspace.py
python3 scripts/validate_documentation.py
cargo test --release --offline --workspace
cargo test --release --offline --workspace --all-features
cargo build --release --offline -p ciagomem-engine --bin ciagom-server
docker build \
  -t ghcr.io/ciago-technologies/elitesand:0.1.1 \
  -t ghcr.io/ciago-technologies/elitesand:v2 \
  -t ghcr.io/ciago-technologies/elitesand:latest \
  .
```

The all-feature release run reported `263 passed, 0 failed`. The validators passed.
Warnings remain visible in the Rust build; a warning-free baseline was not established.

The quickstart and durable recovery examples also passed:

- quickstart created actor `1` and read credits value `75`;
- durable host wrote `100`, mutated to `75`, checkpointed, restarted, and recovered `75`.

### Release and package publication

The annotated Git tag exists:

```text
v0.1.1 -> 79d3ea1
```

GitHub releases:

- `0.1.1 Stable`: `https://github.com/Ciago-Technologies/elitesand/releases/tag/v0.1.1`
- `0.1.0 (Deprecated)`: `https://github.com/Ciago-Technologies/elitesand/releases/tag/v0.1.0`

GHCR image tags were pushed:

```text
ghcr.io/ciago-technologies/elitesand:0.1.1
ghcr.io/ciago-technologies/elitesand:v2
ghcr.io/ciago-technologies/elitesand:latest
```

The published image digest observed during the push was:

```text
sha256:b77574807ff53b0c00e119741e3e9f174d9871490f901efc140f7f737199213f0
```

The previous `v0.1.0` release was not deleted; it was marked deprecated for traceability.

## 3. Branch and PR state

Remote branches before final promotion were linear:

```text
feat/core-mem-server-engine -> dev -> staging -> prod -> main
```

Direct pushes to protected branches were rejected by GitHub rules. The release feature
commit was successfully pushed to `feat/core-mem-server-engine`, and the tag was pushed.

Open promotion PRs:

|                                                           PR | Head                          | Base      | Purpose                  |
| -----------------------------------------------------------: | ----------------------------- | --------- | ------------------------ |
| [#8](https://github.com/Ciago-Technologies/elitesand/pull/8) | `feat/core-mem-server-engine` | `dev`     | Promote 0.1.1 to dev     |
| [#7](https://github.com/Ciago-Technologies/elitesand/pull/7) | `feat/core-mem-server-engine` | `staging` | Promote 0.1.1 to staging |
| [#6](https://github.com/Ciago-Technologies/elitesand/pull/6) | `feat/core-mem-server-engine` | `prod`    | Promote 0.1.1 to prod    |
| [#5](https://github.com/Ciago-Technologies/elitesand/pull/5) | `feat/core-mem-server-engine` | `main`    | Promote 0.1.1 to main    |

The GitHub Actions `build` runs associated with these PRs completed successfully. The PRs
still need the repository's required approvals and merges. Because promotion order matters,
merge them sequentially:

1. PR #8: feature branch -> `dev`
2. After #8 merges, create or update a `dev` -> `staging` promotion PR.
3. After staging is validated, merge staging -> `prod`.
4. After prod is validated, merge prod -> `main`.

The already-open direct feature-to-all-branch PRs may be closed and replaced with sequential
promotion PRs if repository policy or reviewers require strict ordering. Do not merge several
parallel promotion PRs if that would bypass environment validation.

## 4. Immediate next actions

Run these commands from a fresh or current clone:

```bash
git fetch --prune --tags origin
git status --short --branch
gh pr list --repo Ciago-Technologies/elitesand --state open
```

Before merging, inspect every PR:

```bash
gh pr view 8 --repo Ciago-Technologies/elitesand
gh pr checks 8 --repo Ciago-Technologies/elitesand
```

Merge only after required human review and successful checks:

```bash
gh pr merge 8 \
  --repo Ciago-Technologies/elitesand \
  --merge \
  --delete-branch=false
```

Then refresh and promote in order:

```bash
git fetch --prune origin
git switch dev
git pull --ff-only origin dev

gh pr create \
  --repo Ciago-Technologies/elitesand \
  --base staging \
  --head dev \
  --title "release: promote dev to staging" \
  --body "Promote the validated 0.1.1 revision from dev to staging."
```

Repeat the same process for `staging -> prod` and `prod -> main`. Never force-push protected
branches and never rewrite the published `v0.1.1` tag.

After final promotion:

```bash
git fetch --prune --tags origin
git log -1 --oneline origin/main
git show --stat --oneline v0.1.1
gh release view v0.1.1 --repo Ciago-Technologies/elitesand
```

If `origin/main` does not contain the intended release commit, the promotion is incomplete.

## 5. Production gates and remaining work

`GATE.md` is the authoritative gate checklist. Its status must not be overstated.

### Gate 0: baseline verification

Current local evidence: **PASS**.

- release workspace tests passed;
- all-feature tests passed;
- formatting passed;
- workspace validator passed;
- documentation validator passed;
- compile-fail documentation tests passed as part of the test suite.

### Gate 1: dependencies and supply chain

Partially complete. Vendor pins and the lockfile are present, but before a production claim:

- run `cargo tree -d`;
- run `cargo audit` or `cargo deny check`;
- verify `legacy-audit` is absent from the release build;
- consider adding the checks to CI.

### Gate 2: hardware and kernel prerequisites

Not fully qualified. Validate the actual deployment host:

- Linux kernel and `io_uring` support;
- HugeTLB reservation;
- dedicated/isolated CPUs;
- NVMe FLUSH/FUA behavior;
- OpenSSL 3.5.9 or the approved supported version;
- data and key permissions.

The Docker image build is not hardware qualification.

### Gate 3: capacity and performance

Not complete. The alpha benchmark is a controlled smoke measurement, not a production
capacity or SLO proof. Still required:

- production NVMe/CPU benchmark;
- p50/p95/p99 latency distributions over repeated runs;
- sustained write/checkpoint load;
- 1M-tenant residency simulation;
- memory/RSS accounting;
- thermal and scheduler conditions;
- documented results in a benchmark artifact.

Do not claim guaranteed `200 ns`, million-user capacity, or a durable/network latency SLO
from the current benchmark.

### Gate 4: durability and power loss

Not complete. Still required:

- WAL recovery across all tail modes;
- repeated simulated power cuts;
- checkpoint A/B atomicity;
- qualified NVMe/filesystem combinations;
- FLUSH/FUA evidence;
- hard power-loss testing.

### Gate 5: security and cryptographic hygiene

Implementation checks exist, but operational qualification remains:

- key ceremony and rotation procedure;
- certificate renewal;
- production secret handling;
- release dependency audit;
- verification that no secrets are present in images, logs, or documentation.

### Gate 6: operational runbooks

Partially complete. Existing `INSTALL.md`, `SECURITY.md`, `DEBUGGING.md`, `TESTING.md`,
and this checkpoint cover some procedures. `GATE.md` still identifies runbooks to complete:

- `DURABILITY.md`;
- `BENCHMARKS.md`;
- `SECURITY_OPS.md`;
- `UPGRADE.md`;
- `MONITORING.md`.

Create these before claiming production readiness.

### Gate 7: distributed/multi-node scope

Explicitly not complete and out of the current single-node release scope. Do not describe
the current release as a complete distributed runtime, SDK publication, or multi-node
capacity solution.

## 6. Important implementation facts

- Workspace crate versions currently show `0.3.0`; GitHub release `0.1.1` is a repository
  release label, not a Cargo crate-version change.
- The server binary target is `ciagom-server`, but the current Dockerfile still copies
  `target/release/ciagomem-server`. This is a known packaging mismatch and must be fixed
  and rebuilt before treating the container as a valid release artifact.
- The alpha benchmark requires Linux, AVX2, `io_uring`, and CPU affinity support.
- The alpha benchmark uses four physical cores after the recent change; host-specific CPU
  numbering must be rechecked on another machine.
- Existing compiler warnings are not release blockers in the current workflow, but they
  should not be silently ignored when diagnosing new changes.
- Local files and artifacts outside Git are not restored by cloning. Keep authoritative
  team documentation inside the repository.

## 7. Files and references

- `GATE.md`: production gate checklist and qualification gaps.
- `README.md`: current capabilities and explicit non-claims.
- `TESTING.md`: runnable tests and test limitations.
- `INSTALL.md`: installation and service prerequisites.
- `DEBUGGING.md`: diagnostics and retry behavior.
- `SECURITY.md`: security boundaries and planned operational policy.
- `docs/VERIFICATION.md`: historical verification evidence and limitations.
- `docs/ECOSYSTEM_BENCHMARK.md`: documented smoke benchmark and raw measurement reference.
- `.github/workflows/rust.yml`: required CI workflow.
- `ciagomem-engine/examples/alpha_benchmark.rs`: four-core alpha benchmark.
- `Dockerfile`: current container packaging.
- `CLAUDE.md`: agent skill-routing instructions.

## 8. Safe continuation rules for the next agent

1. Read this file, `GATE.md`, and `README.md` before changing code.
2. Run `git status --short --branch` before editing.
3. Do not discard unrelated working-tree changes.
4. Do not force-push, reset hard, or rewrite release tags.
5. Use pull requests for protected branches.
6. Validate the exact requirement rather than relying on a proxy benchmark.
7. Keep production claims limited to observed evidence.
8. Add tests and directly related documentation for code changes.
9. Run the smallest relevant checks, then the full CI-equivalent suite before release.
10. Record new benchmark hardware, configuration, commit SHA, and limitations.

## 9. Definition of done for the pending release promotion

The `0.1.1` promotion is complete only when all are true:

- [ ] PR #8 or an equivalent reviewed feature-to-dev PR is merged.
- [ ] `dev -> staging` promotion is reviewed, checked, and merged.
- [ ] `staging -> prod` promotion is reviewed, checked, and merged.
- [ ] `prod -> main` promotion is reviewed, checked, and merged.
- [ ] `origin/main` contains the intended release commit.
- [ ] `v0.1.1` still points to the intended immutable release commit.
- [ ] GitHub release `0.1.1 Stable` is visible.
- [ ] `0.1.0` remains visible and marked deprecated.
- [ ] GHCR tags `0.1.1`, `v2`, and `latest` resolve to the published image.
- [ ] Release notes disclose benchmark and production-qualification limitations.
- [ ] Dockerfile binary path is corrected to the actual `ciagom-server` target and the image
      is rebuilt and smoke-tested from the corrected source.
- [ ] No secrets or unintended generated files are included.
