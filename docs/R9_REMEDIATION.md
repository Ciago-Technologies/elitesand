# R9 remediation — 31 October 2026

Input: **Ciagomem-SLAE R8 Remediation Verification (R9).md** and `ciagomem-r9-fix-patch.zip` under New File/. Original patch/probes/full files and external logs are preserved under [review/r9](../review/r9). Before editing, the lifecycle probe reproduced SIGSEGV in a bounded child process with core dumps disabled; the two WAL probes reproduced dropped-buffer/rebinding and direct publisher bypass behavior. No real device was used.

The supplied patch applied cleanly and passed 134 default / 162 all-feature tests. Three additional regressions failed on that patch (release/resubmit and lifecycle/log submission unwind). Those were fixed, with rejection-path and completion-boundary checks added; current totals are in [VERIFICATION.md](VERIFICATION.md).

| Finding | Correction and evidence | Remaining boundary |
| --- | --- | --- |
| R9-1 — owners free buffers during pending I/O | Arena counters track successful lifecycle submissions and validated completions. Dropping a lifecycle with pending I/O fences its core; dropping its arena retains the backing when the count is nonzero. Dropping a non-quiescent log fences the core and retains its allocated buffers. Late simulated reads/writes therefore reference live memory. Normal completion returns counts to zero and normal teardown remains reclaimable. Tests cover pending read/write drop, late completion, quiescent drop and release after simulator shutdown. | This is a fail-safe retention fallback, not cancellation or recovery. Abandoned backing/buffers remain allocated until process exit, even if the completion arrives later. Repeated abandonment can exhaust memory. A production reactor must own the ring and buffers and cancel/reap before teardown. Do not reuse a fenced domain or its device file while old I/O may still run. |
| R9-2 — arena holders can revoke another publisher | Direct publish/bind/unbind helpers are crate-private. A non-Clone DurabilityPublisher is acquired by a successful claim and releases that claim in Drop; the WAL owns it through initialization and runtime. Failed/out-of-range claims cannot release another owner's claim. External arena holders cannot call raw publish/unbind; two compile-fail doctests guard that API. Tests retain duplicate/stale-open checks and verify the publisher role remains held after a refused claim. | The publisher holder still supplies the proven durable prefix; this type is an exclusive authority, not a storage verifier. Node construction, arena registration and persistent recovery metadata remain absent. Public fence remains available because it fails closed. |

## Additional lifetime edge found locally

The supplied log release flag stayed true after a later successful submission. A valid unsafe acknowledgement for previously cancelled simulator operations followed by another write could then allow drop to free the new write's buffer. The new `r9_1_new_submission_cannot_reuse_an_earlier_device_release` regression failed before the correction. Each successful write submission now clears the old acknowledgement. The test drops the owner and completes the new write safely from retained memory.

`release_device_references` remains unsafe and intended for teardown. The host must prove all relevant operations have terminated and cannot touch the buffers; closing a handle without that proof is insufficient. In-tree simulator callers explicitly abandon or terminate their operations before acknowledging. A release of old operations never authorizes freeing memory referenced by future submissions.

## Submission failure and completion boundaries

Two additional failing regressions showed that arming lifetime tracking only after the backend returns misses a backend that queues I/O and then unwinds. Lifecycle pending counts and WAL InFlight state are now provisionally armed before submission. A rejected submission rolls them back; an unwind conservatively retains/fences. Tests exercise both read and checkpoint submission unwind, WAL submission unwind, and rejected submissions including a previously acknowledged teardown. The backend contract explicitly requires Err(SqFull) to retain no buffer reference.

CQE handlers are now unsafe: matching tokens/results must prove the backend has completed the request and stopped accessing its buffer. Arbitrary numeric tokens cannot release pending-memory protection through a safe call. Existing callers use actual simulator completions or provably nonmatching stale tokens; two compile-fail doctests guard this boundary. The production driver must establish that proof before invoking a handler.

## Scope and compatibility

No dependency or persistent-format change. CoreControl remains 256 bytes; SlotHeader remains 128. The arena adds one 8-byte pending-I/O counter, not an extra field per slot. Dirty tracking remains 33 bytes per dense slot. Normal native tests do not establish global leak-freedom or measured RSS; deliberate abandonment tests intentionally retain memory.

CoreControl's direct writer methods become crate-private. External writer fixtures and the WAL use `slot::DurabilityPublisher`. Lifecycle exposes quiescent/pending diagnostics and an unsafe device-release acknowledgement; the log has the matching unsafe acknowledgement. Both on_cqe entry points now require the caller's unsafe completion proof. Forgotten owners and abnormal shutdown are fail-safe for the tested drop path, but this is not a general soundness audit of all raw-buffer APIs or an implemented production shutdown driver.

The R8 report moved to review/reports and the superseded R8 input/output ZIPs were removed. No additional obsolete extracted directories were present. One policy.json and one current PERPLEXITY_REVIEW_PROMPT.md remain. The [cleanup manifest](cleanup-manifest.json) records cumulative removals, preserved source hashes and the newly removed archive hashes.

## Evidence limits

The external R9 reviewer reports Miri on the supplied patch's acceptance subsets and full core library, plus an aarch64 compile check. Leak suppression was used only for tests deliberately retaining abandoned allocations; its full library run did not suppress leaks. Those are attributed reviewer results, not local verification of our added flag-reset change. Only stable Rust is installed locally. See VERIFICATION.md for actual local tests, Clippy, formatting and packaging checks.

Still missing: a ring-owning node/reactor, cancel-and-reap shutdown, real io_uring teardown qualification, a GroupCommitLog-backed engine Journal/replay/repair service, production identity anchor, durable checkpoint catalog, transport authentication, model checking, long fuzz campaigns, ARM execution and storage power cuts. Engine MemoryJournal is non-durable. No production, million-user or SQL-speed guarantee follows from this component fix.
