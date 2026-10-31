# Ciagomem self-audit of the R10 working tree

Date: 2026-10-31. This audits the local working tree, including its uncommitted R10 changes. Source hashes and Git status are included in the evidence archive. No implementation changes were made during this audit.

What this repo does: Ciagomem contains an actor-oriented in-memory reference engine and separate storage, lifecycle, security, parser and WAL components. Its distributed operation, external SDKs and production host integration remain planned contracts. This review assumes concurrent actor activity, asynchronous I/O, slot reuse and delayed completion notifications; it does not assume that million-user performance has been demonstrated.

**Verdict: fix findings 1–3 before treating the lifecycle API as safe for production integration.** Existing suites pass, but three additional probes reproduce incorrect behavior. These probes pass when the defects exist; they are demonstrations, not acceptance tests for a fixed implementation.

## Must fix

### 1. High: public arena writes bypass checkpoint protection

- **Location:** `ciagomem-core/src/slot.rs:660` (`write`), `:568` (`payload`); `ciagomem-core/src/lifecycle.rs:906` (`start_flush`).
- **What this is:** The lifecycle stamps an image checksum and lends the slot buffer to asynchronous I/O. Its safety argument requires the buffer to remain unchanged until completion.
- **Problem:** A caller on the same owner core can call the safe public `SlotArena::write` while the lifecycle is `Flushing`. The seqlock does not enforce the lifecycle freeze. The public atomic payload accessor also exposes direct mutation.
- **Reproduction:** `safe_arena_write_bypasses_dma_freeze` starts a checkpoint at LSN 5, writes a body word at LSN 6 through `arena.write`, then completes the genuine simulated write. The disk contains the changed word, image validation fails, and the lifecycle reports a clean resident at LSN 5. No forged completion is used.
- **Smallest complete fix:** Enforce slot I/O ownership at the common arena access boundary and remove unrestricted safe mutable payload/header access. An opaque guard can authorize owner mutations only while no conflicting I/O owns the buffer. Gating only `SlotLifecycle::mutate` cannot close this escape path. Review safe reads during asynchronous loads too.
- **If skipped:** Checkpoints can become corrupt and the lifecycle can lose track of a mutation. Native I/O would also violate the buffer-access contract; native DMA corruption or undefined behavior was not executed in this environment.
- **Acceptance test:** Arena mutation during a pending checkpoint is rejected, the completed image validates, and its body and checkpoint LSN agree. Direct atomic payload access must not offer a safe bypass.

### 2. High: 65,536 pins wrap the pin counter and allow eviction

- **Location:** `ciagomem-core/src/lifecycle.rs:134` (`pins: u16`), `:860` (`pin`).
- **What this is:** Pins keep slot memory alive while zero-copy users still reference it.
- **Problem:** `pins += 1` is unchecked. In the release profile, 65,536 successful pins wrap the counter to zero. Debug overflow checks would panic instead.
- **Reproduction:** `pins_wrap_then_live_slot_is_freed` checkpoints a slot, successfully pins it 65,536 times without releasing any pin, and runs eviction. The free-slot count becomes one.
- **Smallest complete fix:** Use checked addition and reject a pin when the counter is exhausted. Preserve the nonzero count until genuine releases arrive; widening alone does not establish a no-wrap invariant.
- **If skipped:** A high-fanout actor can be freed while readers or zero-copy sends still reference its memory. This probe demonstrates premature freeing, not a live kernel DMA use-after-free.
- **Acceptance test:** The first 65,535 pins succeed, the next fails, and eviction cannot free the slot until every accepted pin is released.

### 3. Medium: stale unpin can consume a different generation's pin

- **Location:** `ciagomem-core/src/lifecycle.rs:871` (`unpin`).
- **What this is:** A completion releases a pin by bare slot index.
- **Problem:** The release API does not check the slot generation or identify the acquisition. A delayed duplicate release can decrement the pin count of an unrelated actor occupying the reused index.
- **Reproduction:** `stale_unpin_releases_new_generation` pins and releases an old actor, evicts it, admits a new actor at the same index with a different handle, and pins the new actor. Repeating the old index-only release permits eviction of the still-pinned new actor.
- **Smallest complete fix:** Return a generation-bound acquisition token and consume each token only once on release. A full `SlotHandle` check addresses cross-generation releases, but does not by itself reject duplicate releases within a generation.
- **If skipped:** A misrouted or duplicate host notification can release another actor's lifetime protection. This is an API-boundary defect; the probe deliberately supplies the stale release and does not establish that the kernel spontaneously duplicates notification CQEs.
- **Acceptance test:** Old-generation and duplicate release tokens are rejected without changing the current actor's pin count.

## Should fix

### 4. CI does not run the project's release and specification gates

- **Location:** `.github/workflows/rust.yml:19`.
- **What this is:** The repository's sole workflow builds and tests the default debug configuration.
- **Problem:** It omits release/all-feature tests, formatting, Clippy and the workspace/specification validator. Release arithmetic differs from debug arithmetic, and optional APIs and specification mirrors are not covered by this workflow.
- **Smallest complete fix:** Add the existing checks to this workflow, including both release configurations and `python3 scripts/validate_workspace.py`. Resolve or explicitly baseline Clippy warnings before making warnings fatal. Keep hardware qualification separate and make hardware skips visible.
- **If skipped:** Local verification can be green while later changes bypass these checks in CI. This finding is based on workflow inspection, not a failing GitHub run.

## Verification performed

All Cargo commands used `CARGO_TARGET_DIR=/tmp/ciagomem-self-audit-target` to leave the repository's build directory untouched.

| Check | Result |
| --- | --- |
| `cargo test --release --offline --workspace` | 151 reported passed; zero failures |
| Same command with `--all-features` | 179 reported passed; zero failures |
| Three external release probes | All three reproduced the defects |
| `cargo clippy --workspace --all-targets --offline` | Completed successfully, with project and dependency warnings; not warning-free |
| `cargo fmt --all -- --check` | Passed |
| `python3 scripts/validate_workspace.py` | Passed, including the specification contract validator |
| `python3 scripts/check_mesh_progress.py` | 1,000 bounded doorbell runs and 25 concurrent FIFO runs passed |
| R10 core/WAL tests rerun with `--nocapture` | Three real-ring test bodies printed `SKIPPED: io_uring unavailable` |

The Cargo pass totals include tests that return successfully when `io_uring` is unavailable. They must not be described as successful real-ring execution. Mesh progress repetitions are not exhaustive concurrency model checking. Specification validation checks encoded layout, grammar, naming and mirror contracts, not distributed correctness or performance guarantees.

## Scope and evidence

The review concentrated on arena access, lifecycle transitions, checkpoint and recovery boundaries, WAL buffer ownership, the simulated and native I/O interfaces, pin lifetime, authorization and the existing regression/CI coverage. The engine, query and crypto tests ran as part of the workspace suites. This was not a line-by-line audit of every vendored dependency.

`ciagomem-self-audit-evidence.zip` contains this report, the standalone probe crate, source fingerprints, the working-tree status and raw verification logs. Extract its `ciagomem-self-audit` directory beside `ciagomem-slae`; its README gives the exact offline command. The archive contains evidence, not a patch or a complete replacement source upload. Keep the existing R10 review ZIP if sharing the full implementation too.

Not checked: Miri, Loom/Shuttle, fuzzing, ARM execution, real NVMe/power-cut behavior, production networking or a distributed deployment. No new latency, capacity or million-user claims are established. Findings 1–3 remain unfixed because this was an audit-only task.
