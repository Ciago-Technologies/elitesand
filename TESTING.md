# Testing, Schema Conformance & Benchmarking

This guide explains how to test your application against CiagoMem in local development and CI/CD pipelines, validate FlatBuffers schema evolution rules, and run reproducible latency benchmarks.

---

## 1. Running Isolated Integration Tests in CI/CD

Because CiagoMem starts in milliseconds and supports isolated namespaces, you can spin up an ephemeral container in GitHub Actions or GitLab CI for end-to-end integration tests:

```bash
# Start an ephemeral CiagoMem test instance in development mode
docker run -d --rm \
  --name ciagomem-test \
  -p 7443:7443 \
  -e CIAGOMEM_MODE=development \
  -e CIAGOMEM_ARENA_GIB=2 \
  -e CIAGOMEM_REACTOR_CORES=2 \
  ciagotech/ciagomem:v0.3.0
```

### Best Practices for Test Isolation

- **Unique Test Namespaces:** Issue a capability token with a distinct `Namespace(u32)` or fresh `Tenant(u128)` incarnation per test suite so parallel test workers never collide on family names (`"users"`) or primary keys (`"user-42"`).
- **Ephemeral `IdentityFile` per Test:** In unit tests, create a temporary `IdentityFile` (e.g., `/tmp/test-client-${uuid}.bin`) for each simulated client so sequence counters start cleanly at `1`.

---

## 2. CI Schema Conformance Testing (`flatc --conform`)

CiagoMem enforces strict binary compatibility across schema versions (`v1 -> v2`) so resident and checkpointed actors never suffer memory corruption during lazy upcasting.

Before calling `client.alterSchema(...)` in production, add the following `flatc` conformance check to your CI pipeline:

```bash
# Verify that app_schema.v2.fbs is 100% binary-compatible with app_schema.v1.fbs
flatc --conform schemas/app_schema.v1.fbs schemas/app_schema.v2.fbs
```

### Schema Evolution Checklist

1. **Explicit IDs Required:** Every field must have an `(id: N)` attribute.
2. **Append-Only Fields:** New fields must be optional and added with the next contiguous ID.
3. **Never Change Types In-Place:** Do not widen (`int32 -> int64`) or narrow a field's type in-place. Instead, deprecate the old field (`old_field: int (id: 2, deprecated);`) and append a new field (`new_field: long (id: 5);`).
4. **Never Reorder Structs:** FlatBuffers `struct` layouts are fixed-size; only `table` definitions can evolve with new optional fields.

---

## 3. Testing Optimistic Concurrency & Idempotency

Your test suite should verify that your application properly handles concurrent actor writes and network retries:

```typescript
import { expect, test } from "vitest";

test("rejects stale document version on concurrent update", async () => {
  const created = await client.create(
    userSchema,
    "user-cas-test",
    initialBytes,
  );

  // First update succeeds (version 1 -> 2)
  const update1 = await client.mutate(
    userSchema,
    "user-cas-test",
    created.actorRef,
    1n, // Expected version 1
    `mutate { state.credits += 50 }`,
  );
  expect(update1.version).toBe(2n);

  // Second update using stale version 1 must fail with Status 5 (VersionConflict)
  await expect(
    client.mutate(
      userSchema,
      "user-cas-test",
      created.actorRef,
      1n, // Stale version!
      `mutate { state.credits -= 20 }`,
    ),
  ).rejects.toMatchObject({ status: 5 });
});
```

---

## 4. Benchmarking & Performance Qualification

When benchmarking CiagoMem on your target hardware, always separate **Registered Tenants**, **Resident Working Set**, and **Active Concurrent Sessions**:

### Target Performance Qualification SLOs (Bare-Metal Linux + PLP NVMe)

| Operation Path                             | Target Latency                   | Measurement Scope                                                     |
| :----------------------------------------- | :------------------------------- | :-------------------------------------------------------------------- |
| **Same-Core Resident Seqlock Read**        | `p99 ≤ 200 ns` / `p99.99 ≤ 1 µs` | Validated in-engine atomic snapshot (excludes network RTT).           |
| **Local `memfd` Sealed Window Read**       | `p99 ≤ 300 ns`                   | Co-located agent reading an already-published 4 KiB projection.       |
| **Single-Actor Durable Commit (`Tier 1`)** | `p99 ≤ 50 µs`                    | Compact WAL v2 group-commit over `io_uring` `O_DIRECT` to PLP NVMe.   |
| **Cold Actor NVMe Fault-In**               | `p99 ≤ 250 µs`                   | A/B extent read, CRC32C verification, and TinyLFU admission into RAM. |

### Differential Index Verification

To measure the exact speedup of Roaring Bitmap filter pushdown and SIMD Zone Maps against a raw scalar scan on your dataset, compare query execution with index pushdown enabled versus disabled in your benchmark harness:

- Ensure at least **10,000+ actors** are populated across multiple shards.
- Record `accounted_bytes`, `hit_rate`, `io_uring_cqe_latency_us`, and `p50 / p99 / p99.99` latencies across sustained 60-second load ramps.
