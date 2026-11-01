# CiaQL reference

**Current:** bounded actor-profile parser and reference executor. **Target:** schema-bound collection/array profile in [specification 17.2](MASTER_SPECIFICATION.md#172-unified-target-ciaql-application-grammar). The complete target grammar is authoritative; do not treat its examples as accepted by the current parser.

## Current actor profile

After loading actors and authorizing a session as in the [complete quickstart](ciagomem-engine/examples/quickstart.rs):

```text
@1 mutate {state.credits -= 25}
@1 select {credits: state.credits}
```

The parser also has ACTOR, TX/TX_ACTORS and SCAN ACTORS sources, typed filtering/projection/grouping, search/similarity, relationship and frame/operator AST forms. Executor support and authorization are separate from parsing success. General arrays, runtime BFBS families and network mutation commands are not implemented by having an Append AST variant.

The current lexer/parser allocates bounded tokens/AST collections. It does not promise zero allocations. Current source limits are64 KiB text,16,384 tokens,32 stages, nesting16, MAX_LIST=256,4096 vector dimensions and2..64 TX actors. Inspect parser and executor errors instead of assuming every syntactically accepted value is executable.

## Target lexical and safety rules

Only `|>` is a pipe; `|->` is invalid. Keywords are ASCII case-insensitive; paths/families/aliases and string values preserve case. Strings use validated escapes; `#` comments end at newline outside strings. Errors carry original half-open UTF-8 byte spans.

Target mutation batches allow128 operations; the129th rejects before logging. Arrays are bounded by their catalog descriptor (default 1024 elements) and extent size. Ordinary lists/columns/group keys are capped256; vectors are1..4096 dimensions; top_k is1..1000, default 10. Admission also enforces query work, output and allocation budgets.

## Target sources and reads

| Source | Meaning |
| --- | --- |
| `@123` or `@0x7b` | Authorized point actor; ID resolves through full incarnation/directory checks |
| `family("users") key "user-42"` | Point key under authenticated namespace |
| `scan family("users")` | Authorized resident+cold family membership, not unrelated families |
| `tx [@1, @2]` | Explicit bounded deterministic participant set |

```text
scan family("users")
  |> filter (state.credits >= 50 and state.status == "ACTIVE")
  |> select {name: state.username, version: meta.document_version}
  |> take 25
```

Engine metadata is read-only: meta.schema_version/family_id are u16; meta.actor_id/document_version are u64. meta.last_tx_seq is not defined. Missing nullable values are null; missing nonnullable scalars yield schema defaults. `null == null` is true. Other null comparisons are false except `!=` with exactly one null. CONTAINS is case-sensitive substring search; IN performs typed equality. Mixed integer/float comparisons must preserve i64 distinctions above 2^53.

## Target mutations and conditional commands

```text
family("users") key "user-42"
  |> where (state.credits >= 25)
  |> mutate {state.credits -= 25, state.tags.append("verified")}
```

| Operation | Contract |
| --- | --- |
| `field = value` | Typed scalar/vector replacement |
| `field += n`, `field -= n` | Checked numeric arithmetic; finite floats |
| `field = null` | Nullable field clear; not an actor deletion |
| `arr.append(v)` | Append under descriptor and extent bounds |
| `arr.remove(i)` | Existing zero-based index, preserving order |
| `arr.pop()` | Remove final element and optionally return it; empty rejects |
| `arr.clear()` | Empty vector; retains actor |
| `arr.push_bounded(v, cap)` | Catalog-approved ring capacity, discard oldest first; return discarded count |

Repeated targets reject. Every operation in one batch succeeds or none does. Typed preparation and reservations precede WAL assignment; no partial indexes, label changes or version increments on failure. Compaction/promotion is planned document-path work, not a generated FlatBuffers getter modifying a vector in place.

Target SDK MUTATE takes one source-free command:

```text
where (state.status == "ACTIVE" and state.balance_cents >= 5000)
mutate {state.balance_cents -= 5000, state.tx_count += 1}
```

This requires negotiated conditional_mutate_v1. Source family/key, expected actor incarnation and expected version arrive in the RPC. False predicate is status6/ConditionFailed; stale version is status5/VersionConflict. See specification 17.3 for durable terminal decisions and [safe retries](DEBUGGING.md).

## Target analytics and retrieval

```text
scan family("orders")
  |> group_by (state.region)
  |> aggregate {count: count(), total: sum(state.amount_cents), p95: p95(state.amount_cents)}
  |> take 20
```

COUNT() counts rows; COUNT(path), COUNT_DISTINCT, SUM, AVG, MIN, MAX and exact nearest-rank P50/P95/P99 ignore null inputs. Integer SUM uses i128 intermediate and checked i64 output. Empty count/sum yield0; other empty results yield null. All group/distinct/sort state is bounded. After aggregation only grouped keys or declared aggregate results may be projected; TAKE is final.

```text
scan family("agent_memories")
  |> search(field: state.transcript, query: "network policy", top_k: 50)
  |> similarity(vector: [0.1, 0.2, 0.3, 0.4], metric: COSINE, threshold: 0.75)
  |> select {text: state.transcript}
  |> take 5
```

The example assumes a registered four-dimensional vector. Stage order changes candidates; RRF combines one-based ranks using1/(60+rank), with actor-ID ties. SQ8 shortlisting does not replace exact final threshold qualification. COSINE/DOT accept scores>=threshold, L2 accepts distance<=threshold. Vectors/thresholds must be finite; dimensions match the descriptor; cosine zero norms reject.

## Target joins, links and deletion

```text
@100 |> join @actor(state.organization_ref) as @org
     |> where (@org.state.active == true)
     |> select {company: @org.state.company_name}
@2 link add(target: @1, on_delete: RESTRICT)
@3 link add(target: @1, on_delete: CASCADE)
delete actor @1
```

Joins use catalog ActorRef fields with incarnations, authorized family scope, unique aliases, bounded fanout and propagated labels. A missing target is an inner-join miss. LINK is a mutation requiring its own idempotent coordinator/RPC; remote QUERY is read-only. TX write sets are explicit; no runtime join may silently add write participants.

RESTRICT checks live referencing actors, not an implicit business-status filter. Cascades are bounded strict transactions or recoverable chunked jobs. Logical deletion and physical A/B reclamation are distinct. Operator export/frame commands remain separate privileged profile operations, not data-query side effects.
