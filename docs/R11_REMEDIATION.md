# R11: local self-audit corrections

The R10 self-audit found three defects and a CI coverage gap. This revision fixes the component APIs and adds acceptance tests. It preserves the earlier R5–R10 recovery/security corrections and the section14–15 implementation roadmap.

| Finding | Correction | Evidence |
| --- | --- | --- |
| R11-1: safe arena mutation during checkpoint | Exclusive lifecycle range claims; CPU access guards; device exclusion before checksum/stamping/submission; raw atomics private | `tests/r11_regressions.rs` checkpoint/load, parked-reader, overlap and abandoned-I/O tests; privacy/view compile-fail tests |
| R11-2: pin counter wraps | Checked u16 addition; saturation refuses acquisition without losing existing pins | 65,535 successful acquisitions, next refused, eviction blocked until all tokens consumed |
| R11-3: stale/duplicate unpin | Private, non-clonable tokens bind acquisition to lifecycle owner and full handle; release consumes token and validates generation | Wrong-lifecycle runtime test, stale-generation unit test and duplicate-consumption compile-fail test |
| R11-4: CI skips release/spec gates | Existing workflow now runs release default/all-features, formatting, validator, Clippy and bounded mesh checks | Workflow inspection and equivalent local commands; hosted Actions execution not claimed |

## API changes for integrators

Use `SlotLifecycle::mutate` for slots owned by a lifecycle. Public `SlotArena::write`, `install`, `revoke`, `set_checkpoint_lsn` and `with_unowned_payload` remain available for unmanaged slots, but reject claimed ranges. Direct `header()` and `payload()` references are crate-private. `read()` still returns validated owned results; its view cannot escape the closure.

`pin` now returns `Option<PinToken>` instead of bool. Store that token with the egress request and pass it by value to `unpin` only on its notification completion. `unpin` returns the token on an ownership/generation error so the correct lifecycle can consume it. Do not drop tokens as a substitute for completion. The old bare-index release API has been removed.

The existing deadline regression now holds a real public arena write section instead of directly reaching into the header. Historical vulnerable APIs remain gated by `legacy-audit`; they are not the production correction path.

## Boundaries and costs

SlotHeader remains 128 bytes/alignment64 and the storage formats are unchanged. A process-local access sidecar adds eight bytes per allocated slot; CPU reads pay atomic guard admission/release. Reads of a buffer currently used by DMA defer, including checkpoint reads. No post-change latency claim is made.

I/O or pin abandonment fences the core and retains the affected claims/backing. This closes reuse hazards but can retain memory until exit. Complete cancellation/reaping in a production host is still required. Pinning protects reuse; unsafe zero-copy transport submission must separately guarantee buffer immutability until its notification.

See [verification results](VERIFICATION.md) and specification section16 for the exact access-word layout and contracts. Schema/SDK/distributed/warm-restart targets remain planned. The original self-audit reproductions are historical defect demonstrations, not acceptance tests for fixed behavior.
