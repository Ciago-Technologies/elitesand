# R10 remediation and specification extension

Incoming report: [preserved R10 verification](../review/r10/Ciagomem-SLAE-R9-Remediation-Verification.md.txt). Input hashes and naming-normalized evidence hashes are in [r10-evidence.json](r10-evidence.json). Supplied patched files are historical reference text, not the active source. The patch was applied as a diff after replacing obsolete crate identifiers; it did not overwrite the newer schema/SDK contracts.

| Finding | Current correction | Local evidence / boundary |
| --- | --- | --- |
| R10-1 checkpoint unwind | Install ticket, Flushing state and pending counters before backend submission; SQ rejection restores the prior state/counters without reusing a ticket | Original logical defect probe reproduced before correction. New unwind/rejection acceptance tests pass. Unwind does not imply the I/O disappeared. |
| R10-2 safe simulator pointer injection | SimIo inflight vector is private, exposed only as a read-only slice; abandon_all models simulated ring destruction | Private-field compile-fail doctest and ordinary simulator submissions pass; the original safe exploit probe now fails compilation for both the private queue and typed wake ID. Deliberate heap corruption/UAF probes were not executed locally. |
| R10-3 fenced WAL retention | Match genuine LogWrite/object/ticket before retiring its buffer; fenced completions never publish or resubmit; retry-exhausted completed buffer becomes reclaimable | Original logical defect probe reproduced before correction. External fence/retry-exhaustion tests pass. Unreaped buffers still require retention. |
| R10-4 forged wake tokens | Safe post_wake accepts a u32 wake ID and always encodes Op::Wake; WAL CQE dispatcher rejects other operation classes | Non-LogWrite matching-bit acceptance test passes. Real-ring tests skip locally; current code does not make violating unsafe on_cqe preconditions safe. |

All callers of the now-private simulator queue were updated. No dependency, SlotHeader ABI, WAL envelope or current checkpoint-format change was made. Slot comments were corrected to the actual 128-byte header, 4-bit class encoding and application-data offset16 words; the layout example now asserts each actual field offset and prints native alignment.

The current release suites report **151 default / 179 all-feature passed**, including new positive acceptance tests and compile-fail guards. Three real-ring tests return early with `SKIPPED: io_uring unavailable` locally; these counted harness successes are **not real-ring validation**. The two supplied pre-fix logical probes assert defect presence and are preserved as text, not installed as positive regression tests. Reviewer Miri/aarch64/real-ring evidence is attributed to that reviewer's patch and host.

Sections14–15 of the specifications define future schema/SDK and distributed protocols. They do not implement a node host, durable application Journal, reactor, replication, migration, warm attach or client SDKs. New v4 extent headers/protocol-v2 requests are explicitly target formats with migration/negotiation requirements; current v3/v1 boundaries remain intact.

Open gates: authoritative node constructor, cancel-and-reap, WAL-backed Engine Journal and replay/repair, independent identity anchor, hardware durability qualification, Miri/model checking/ARM execution, consensus/fencing, budget enforcement and measured workload capacity. A quorum/write CQE is not itself a complete storage or distributed correctness proof.

The review's informational notes remain explicit boundaries: device-reference release is teardown-only; R10 ensures at most one lifecycle I/O per slot so the existing u8 counter is not expanded; ring close alone is never a completed cancel-and-reap operation.
