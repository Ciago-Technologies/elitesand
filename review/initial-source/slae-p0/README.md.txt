# slae-p0

P0 fixes for Ciagomem-SLAE. Design rationale: `Ciagomem-SLAE-P0-Fix-Specification.md`.

| Module | Content |
|---|---|
| `seqlock` | Fence-based seqlock (relaxed 8-byte loads + acquire fences; read-only-mapping safe) |
| `mesh` | N x N SPSC core mesh with doorbells, Dekker parking, all-or-nothing admission |
| `uring` | io_uring backend (fixed files/buffers, RWF_DSYNC, MSG_RING wakeups) |
| `slot` | `SlotHandle` (gen:40/class:4/idx:20), 64 B `SlotHeader`, HugeTLB memfd arena, 16 B route table |
| `lifecycle` | CQE-gated load/checkpoint/eviction state machine (A/B extent copies) |
| `gclog` | Per-core group-commit log, prefix durability, Pad/Rewrite tails, recovery, truncation |
| `envelope` | CRC32C (SSE4.2/ARMv8), record and extent image formats |
| `io` | `IoBackend` trait, `user_data` encoding, deterministic fault-injecting simulator |
| `calvin` | `full_mask`, tagged vote table with unique decider |
| `ciaql` | Lexer, parser, validator; spec examples verbatim |

```
cargo test --release
cargo run --release --example spec_regressions
cargo run --release --example wa_model
MIRIFLAGS="-Zmiri-seed=1" cargo +nightly miri test --lib -- seqlock mesh calvin slot ciaql envelope
```
