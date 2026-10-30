# Actor-native execution benchmark

R5 revision measurement. Observed on 31 October 2026 using Rust 1.99.1, release optimization, a single unpinned Linux x86_64 process and an in-memory journal. Four partitions are simulated in-process; these are not four concurrent reactor threads.

Command:

```sh
cargo run --release --offline -p ciagomem-engine --example benchmark
```

The example inserts 10,000 one-field records, warms the indexed path, parses once, then runs 100 queries for `state.value == 777` with projection. Both modes return the same one-row result. Timing excludes initial insertion, parsing and authentication-token verification. Bitmap and scalar measurements run consecutively, so cache/order effects are possible; this is a reproducible smoke measurement, not a statistically controlled SLO benchmark.

| Path | Observed average | Instrumented work units |
| --- | ---: | ---: |
| Bitmap candidate pushdown | 1,130 ns/query | 7 |
| Scalar path in the same executor | 5,294,179 ns/query | 20,001 |
| Snapshot handle lookup/pin, 100,000 calls | 27 ns/call | Not a complete query |

Reported actor/index byte estimate: **10,260,000 bytes**, excluding independent MemoryJournal retention, external pinned snapshots and process/kernel overhead. This is not measured RSS or a one-million-user capacity result.

See [raw measurements and source hashes](ecosystem-benchmark.json) and [benchmark source](../ciagomem-engine/examples/benchmark.rs).

The indexed/scalar difference demonstrates selective candidate elimination inside CiagoMem. It does **not** establish a speedup against an indexed SQL database, Redis, a vector database or an AI-memory service. Snapshot timing does not include parsing, network transport, payload projection, durable I/O, remote joins or LLM execution. No universal sub-200 ns query claim follows from it.

A fair external comparison still needs equivalent schemas/indexes, datasets, selectivity, query semantics, durability, concurrency, memory/RSS accounting, warm/cold conditions and latency distributions over a sustained run.
