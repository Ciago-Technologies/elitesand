# Debugging, Logging & Troubleshooting

This guide covers how to enable verbose debugging in the CiagoMem SDKs, attach custom structured loggers, inspect crash-safe idempotency state, and resolve common wire status codes.

---

## 1. Enabling SDK Debug Mode

Debugging CiagoMem client interactions starts with enabling the `debug` option in your client configuration.

```typescript
import { Client, IdentityFile } from "@ciagomem/sdk";

const client = await Client.connect({
  transport: {
    kind: "tls",
    host: "db.example.internal",
    port: 7443,
    ca: caCertBytes,
  },
  token: macaroonBytes,
  identity: await IdentityFile.open("client-state.bin"),
  debug: true, // Enables verbose wire-frame, sequence, and latency logging
});
```

Enabling `debug: true` outputs detailed diagnostic traces for every request and response frame, including:
* 18-byte wire header decoding (`stream_id`, `op`, `payload_len`, and header `CRC32C` verification).
* Capability renewal and `BootEpoch` negotiation during `HELLO`.
* Outgoing `client_id` and `client_seq` idempotency pairs.
* Server execution latency, `commit_lsn`, and `catalog_epoch` transitions.

---

## 2. Customizing the SDK Logger

In production services, you can override the default console output by supplying a custom `logger` implementation to route SDK events to Pino, Winston, Datadog, or OpenTelemetry:

```typescript
import log from "./my-logging-service";
import { Client } from "@ciagomem/sdk";

const client = await Client.connect({
  transport: tlsConfig,
  token: macaroonBytes,
  identity: identityFile,
  logger: {
    error(code, metadata) {
      log.error({ code, ...metadata }, "CiagoMem SDK Error");
    },
    warn(code, metadata) {
      log.warn({ code, ...metadata }, "CiagoMem SDK Warning");
    },
    debug(code, metadata) {
      log.debug({ code, ...metadata }, "CiagoMem Wire Trace");
    },
  },
});
```

---

## 3. Debugging CiaQL Syntax & Span Errors

When a CiaQL query fails lexical or semantic validation, CiagoMem returns a structured diagnostic containing the exact **UTF-8 byte span `[start, end)`** of the offending token or field path:

```typescript
try {
  await client.query(`scan family("users") |> filter (state.non_existent_field > 10)`);
} catch (err: any) {
  console.error("Status Code:", err.status);      // 4 (SchemaMismatch) or 6 (InvalidArgument)
  console.error("Error Span:", err.span);         // { start: 32, end: 56 }
  console.error("Server Detail:", err.detail);    // Unknown field 'non_existent_field' in family 'users' v1
}
```

---

## 4. Inspecting & Recovering `IdentityFile` State

Every mutating SDK call (`create`, `upsert`, `mutate`, `delete`) is protected by a local crash-safe sequence store (`IdentityFile`). Before transmitting a mutation, the SDK fsyncs `(client_id, client_seq, request_digest, payload_bytes)` to disk.

If your application crashes or loses network connectivity mid-flight (`Status 13: Indeterminate`), **do not delete your `client-state.bin` file!**
1. When your application restarts and calls `Client.connect(...)` with the same `IdentityFile`, the SDK automatically detects the pending unacknowledged mutation.
2. It re-transmits the exact request with the original `client_seq` and `request_digest` (updating only the renewed authentication token if needed).
3. The server checks its durable **72-byte Result Ledger**: if the mutation already committed before the crash, the server returns the original committed `WriteResult` without re-running the mutation.

---

## 5. Troubleshooting Common Status Codes

| Status Code | Error Name | Root Cause | Resolution |
| :--- | :--- | :--- | :--- |
| **`Status 3`** | `PermissionDenied` | 1. Token expired (`ExpiresAt`).<br>2. Server restarted and generated a new `BootEpoch`.<br>3. Token lacks `AllowedFamilies` or `Ops` bit. | Fetch a freshly minted Macaroon from your auth issuer using the server's current `BootEpoch` (returned by `HELLO`). |
| **`Status 4`** | `SchemaMismatch` | 1. Client built a FlatBuffer using an unregistered `.bfbs` version.<br>2. `file_identifier` (e.g., `"USER"`) is missing from `builder.finish()`. | Ensure `builder.finish(root, "USER")` includes the 4-byte identifier and matches the registered `SchemaCatalog` fingerprint. |
| **`Status 5`** | `VersionConflict` | Concurrent writer updated the actor; `expected_document_version` no longer matches. | Read the latest actor snapshot (`client.read(...)`), recompute your changes against `doc.version`, and retry. |
| **`Status 8`** | `Busy` | 1. Actor is paused in a `FreezeCheckpoint` barrier.<br>2. DMA owns the slot (`WriterActive`).<br>3. Per-actor deferral queue (64 requests) is full. | Retry with exponential backoff (e.g., 2 ms, 5 ms, 10 ms) using the **same** `client_seq`. |
| **`Status 9`** | `IdempotencyConflict` | Client reused an existing `client_seq` with different mutation arguments or target key. | Never manually edit `IdentityFile` or share a single `IdentityFile` across multiple concurrent processes. |
| **`Status 10`** | `RetryTooOld` | Client retried a `client_seq` older than the 24-hour dedup retention window. | Open a new `client_id` session and read the current actor state before issuing new writes. |
| **`Status 12`** | `NeedsRepair` | Checksum or epoch mismatch detected on disk extent during cold load. | Actor is automatically fenced to prevent corruption propagation. Restore from replica or A/B backup. |