# CiaQL Language Reference

**CiaQL (Ciagomem Query and Cognitive Language)** is a strongly typed, bounded, pipeline-oriented language for querying, mutating, joining, and analyzing actor state and AI memory frames.

CiaQL is **not SQL**. It is purpose-built for actor-native storage, guaranteeing predictable memory bounds and zero-allocation hot paths.

---

## 1. Lexical & Structural Rules

* **Pipeline Operator (`|>`):** Stages in a CiaQL query are chained using the pipe operator (`|>`). Each stage consumes the candidate set or state produced by the previous stage.
* **Case Sensitivity:** Keywords (`scan`, `filter`, `where`, `mutate`, `select`, `group_by`, `aggregate`, `take`, `and`, `or`, `not`) are ASCII case-insensitive. Family names (`"users"`) and field paths (`state.credits`) are strictly **case-sensitive**.
* **Field Roots:** State fields are referenced via `state.<field_name>`. Engine metadata fields are referenced via `meta.<field_name>` (e.g., `meta.schema_version`, `meta.last_tx_seq`).
* **Hard Safety Bounds:**
  * Maximum query text length: **64 KiB**
  * Maximum tokens per query: **16,384 tokens**
  * Maximum vector literal dimensions: **4,096 dimensions**
  * Maximum `top_k` bound: **1 to 1,000** (defaults to `10` when omitted)
  * Maximum mutations per batch: **128 operations**

---

## 2. Query Sources

Every CiaQL pipeline begins with a source selector:

| Source Syntax | Scope | Description |
| :--- | :--- | :--- |
| `@12345` or `@0x0008F421` | Single Actor | Targets a specific actor by its 64-bit decimal or hexadecimal ID. |
| `family("users") key "user-42"` | Single Actor | Resolves an actor by its collection name and UTF-8 primary key. |
| `scan family("users")` | Family Scan | Scans all live actors (resident and cold) in the `"users"` family within the authenticated namespace. |
| `tx [@1, @2, @3]` | Multi-Actor Tx | Declares a deterministic multi-actor transaction participant set. |

---

## 3. Filtering (`filter` / `where`)

Filter stages evaluate boolean predicates. Leading equality and range filters are automatically pushed down into per-tenant **Roaring Bitmaps** and **64-slot SIMD Zone Maps** before materializing actor snapshots.

```text
scan family("users")
  |> filter (state.status == "ACTIVE" and (state.credits >= 500 or state.tier == "ENTERPRISE"))
  |> filter (not state.suspended == true)
```

### Supported Comparison Operators
* Equality: `==`, `!=`
* Ordering: `<`, `<=`, `>`, `>=`
* Set & Substring (Case-insensitive): `IN`, `CONTAINS`

### Null & Type Semantics
* Missing optional FlatBuffer fields evaluate to `null`.
* `null == null` evaluates to `true`.
* Mixed integer/float comparisons preserve exact `i64` precision above $2^{53}$ without silent IEEE-754 rounding.

---

## 4. Mutations (`mutate`)

Mutations execute atomically on the owner core. All numeric arithmetic (`+=`, `-=`) is overflow-checked before logging.Repeated targets in the same `mutate` block are rejected to prevent ordering ambiguity.

```text
family("users") key "user-42"
  |> where (state.credits >= 25)
  |> mutate {
       state.credits -= 25,
       state.email = "new_email@ciagotech.com",
       state.tags.append("verified"),
       state.audit_log.push_bounded("credits_deducted", 100)
     }
```

### Supported Mutation Operations

| Syntax | Target Type | Description |
| :--- | :--- | :--- |
| `state.field = <scalar>` | Scalar (`bool`, `i64`, `f32`, `f64`, `string`, `ActorRef`) | Sets the field to a new scalar value. |
| `state.field += <number>` | Numeric (`i64`, `f32`, `f64`) | Checked addition assignment. |
| `state.field -= <number>` | Numeric (`i64`, `f32`, `f64`) | Checked subtraction assignment. |
| `state.field = null` | Optional Scalar | Clears an optional field to `null`. Fails on required fields. |
| `state.arr = [v1, v2]` | Vector (`[i64]`, `[f32]`, `[string]`) | Replaces the entire vector. |
| `state.arr.append(v)` | Vector | Appends one element to the end of the vector (max 1,024 elements default). |
| `state.arr.remove(idx)` | Vector | Removes the element at 0-based `uint32` index `idx`. |
| `state.arr.pop()` | Vector | Removes and returns the final element. Fails with `EmptyArray` if empty. |
| `state.arr.clear()` | Vector | Empties the vector (`len = 0`). |
| `state.arr.push_bounded(v, cap)` | Ring Vector | Appends `v` and evicts oldest elements if length exceeds `cap`. Returns count of evicted elements. |

---

## 5. Projections (`select`) & Limiting (`take`)

Project specific fields into named output columns and bound the returned row count:

```text
scan family("users")
  |> filter (state.credits > 0)
  |> select {
       user_name: state.username,
       current_balance: state.credits,
       schema_ver: meta.schema_version
     }
  |> take 25
```
*Note: `take N` must be the final stage in a pipeline (except when followed by an operator `export` stage).*

---

## 6. Grouping (`group_by`) & Aggregations (`aggregate`)

Execute bounded analytical aggregations across a family scan:

```text
scan family("orders")
  |> filter (state.status == "COMPLETED")
  |> group_by (state.region, state.plan_tier)
  |> aggregate {
       order_count: count(),
       total_revenue: sum(state.amount_cents),
       avg_order: avg(state.amount_cents),
       min_order: min(state.amount_cents),
       max_order: max(state.amount_cents),
       unique_customers: count_distinct(state.customer_id),
       p95_amount: p95(state.amount_cents)
     }
  |> take 20
```

### Supported Aggregate Functions
* `count()`: Counts all matching rows (includes nulls).
* `count_distinct(path)`: Exact distinct count of non-null scalar values.
* `sum(path)`: Accumulates integers in `i128` (rejecting `i64` result overflow) or finite floats.
* `avg(path)`, `min(path)`, `max(path)`: Excludes nulls.
* `p50(path)`, `p95(path)`, `p99(path)`: Exact nearest-rank quantiles.

---

## 7. Cross-Actor Joins (`join`) & Referential Links (`link`)

### Direct Actor Reference Join
```text
@100
  |> join @actor(state.organization_ref) as @org
  |> where (@org.state.active == true)
  |> select {
       employee: state.username,
       company: @org.state.company_name
     }
```

### Referential Integrity Links
```text
# Register a RESTRICT constraint (blocks deleting @1 while @2 exists)
@2 link add(target: @1, on_delete: RESTRICT)

# Register a CASCADE constraint (deleting @1 automatically deletes @3)
@3 link add(target: @1, on_delete: CASCADE)
```

---

## 8. Full-Text (`search`) & Vector Similarity (`similarity`)

Stage order is semantic: placing `search` with `top_k: 50` before `similarity` shortlists the top 50 BM25 text matches before running vector scoring. When both stages are present, scores are combined using **Reciprocal Rank Fusion (`1 / (60 + rank)`)** with deterministic actor-ID tie-breaking.

```text
scan family("agent_memories")
  |> filter (state.active == true)
  |> search(field: state.body, query: "zero trust control plane", top_k: 50)
  |> similarity(vector: [0.15, -0.42, 0.77, 0.09], metric: COSINE, threshold: 0.75)
  |> select {
       snippet: state.body
     }
  |> take 10
```

* **Supported Metrics:** `COSINE` (score $\ge$ threshold), `DOT` (score $\ge$ threshold), `L2` / Euclidean (distance $\le$ threshold).