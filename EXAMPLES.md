# Developer Guide: Schemas, SDKs & CiaQL Examples

> **Implementation status:** This guide includes target architecture and acceptance API examples. Current runnable integration is the [embedded Rust quickstart](ciagomem-engine/examples/quickstart.rs), using non-durable MemoryJournal. Runtime BFBS families, general arrays, network SDKs/server and distributed services remain milestones in specification 14.11. Section 17 is the complete target application/operations contract; target examples are not runnable SDK releases or measured performance guarantees.


This guide walks through the complete developer workflow in **Ciagomem — SLAE**: designing a FlatBuffers schema, registering it with the engine, performing zero-copy CRUD operations across TypeScript, Go, and Rust, and writing advanced **CiaQL** pipelines.

---

## 1. Target: Defining & Compiling Your Schema

In the target application profile, every collection (**Family**) is backed by a strictly typed FlatBuffers schema.

### Step 1.1: Create `app_schema.fbs`
Every field in your root table **must** declare an explicit, contiguous `(id: N)` attribute. Note that FlatBuffers uses `long` for 64-bit signed integers (`i64`) and `[float]` for vector embeddings.

```flatbuffers
namespace ciagomem.app;

table UserProfile {
  username: string (id: 0);
  email: string (id: 1);
  credits: long = 0 (id: 2);
  tags: [string] (id: 3);
  memory_embeddings: [float] (id: 4);
}

root_type UserProfile;
file_identifier "USER";
```

### Step 1.2: Compile Binary Reflection (`.bfbs`) & Language Bindings
Run `flatc` outside the database server during your build or CI/CD pipeline:

```bash
# 1. Generate the binary reflection schema (.bfbs) for Ciagomem server registration
flatc --binary --schema -o build app_schema.fbs

# 2. Generate language-specific zero-copy accessors
flatc --ts -o generated/ts app_schema.fbs
flatc --go -o generated/go app_schema.fbs
flatc --rust -o generated/rust app_schema.fbs
```
*The `build/app_schema.bfbs` file is ingested by Ciagomem at runtime (`REGISTER_SCHEMA`), allowing the server to validate offsets, enforce field bounds, and execute CiaQL queries without recompiling the database.*

---

## 2. TypeScript / Node.js SDK Complete Workflow

The `@ciagomem/sdk` package communicates over multiplexed TLS 1.3 (or local Unix `SOCK_SEQPACKET`) and uses a crash-safe `IdentityFile` to guarantee exactly-once mutation idempotency across network retries.

```typescript
import { readFile } from "node:fs/promises";
import * as flatbuffers from "flatbuffers";
import { UserProfile } from "./generated/ts/ciagomem/app/user-profile.js";
import { Client, IdentityFile } from "@ciagomem/sdk";

async function main(): Promise<void> {
  // 1. Open crash-safe client sequence store & connect over TLS 1.3
  const identity = await IdentityFile.open("ts-client-state.bin");
  const client = await Client.connect({
    transport: {
      kind: "tls",
      host: "db.example.internal",
      port: 7443,
      serverName: "db.example.internal",
      ca: await readFile(process.env.CIAGOMEM_CA_FILE!),
      minVersion: "TLSv1.3",
      rejectUnauthorized: true,
    },
    token: await readFile(process.env.CIAGOMEM_TOKEN_FILE!),
    identity,
  });

  try {
    // 2. Register Schema (Idempotent: returns existing descriptor if fingerprint matches)
    const schema = await client.registerSchema(
      "users",
      1,
      await readFile("build/app_schema.bfbs")
    );

    // 3. Construct FlatBuffer Payload
    const builder = new flatbuffers.Builder(256);
    const usernameOffset = builder.createString("anuj_tiwari");
    const emailOffset = builder.createString("anuj@ciagotech.com");
    const tagsOffset = UserProfile.createTagsVector(builder, [
      builder.createString("admin"),
      builder.createString("developer"),
    ]);
    const embeddingOffset = UserProfile.createMemoryEmbeddingsVector(
      builder,
      [0.12, -0.88, 0.44, 0.05]
    );

    UserProfile.startUserProfile(builder);
    UserProfile.addUsername(builder, usernameOffset);
    UserProfile.addEmail(builder, emailOffset);
    UserProfile.addCredits(builder, 100n); // 64-bit integers use BigInt (100n)
    UserProfile.addTags(builder, tagsOffset);
    UserProfile.addMemoryEmbeddings(builder, embeddingOffset);
    const root = UserProfile.endUserProfile(builder);
    builder.finish(root, "USER");

    // 4. CREATE Actor ("user-42") in Family "users"
    const created = await client.create(schema, "user-42", builder.asUint8Array());
    console.log(`Created Actor ID: ${created.actorRef.id}, Version: ${created.version}`);

    // 5. UPSERT Actor (Unconditional replacement or compare-and-swap)
    await client.upsert(schema, "user-42", builder.asUint8Array());

    // 6. READ Actor (Zero-Copy FlatBuffer Access)
    const doc = await client.read(schema, "user-42");
    const user = UserProfile.getRootAsUserProfile(new flatbuffers.ByteBuffer(doc.bytes));
    console.log(`Username: ${user.username()}, Credits: ${user.credits()}`); // 100n

    // 7. ATOMIC MUTATE via CiaQL (Appends to extent delta tail without sending full blob)
    const updated = await client.mutate(
      schema,
      "user-42",
      created.actorRef,
      doc.version, // Optimistic concurrency precondition
      `mutate {
        state.credits -= 25,
        state.tags.append("active_session")
      }`
    );
    console.log(`Mutated to Version: ${updated.version} at LSN: ${updated.lsn}`);

    // 8. QUERY Family via CiaQL Pipeline
    const rows = await client.query(`
      scan family("users")
        |> filter (state.credits > 50)
        |> group_by (state.username)
        |> aggregate {
             total_users: count(),
             avg_credits: avg(state.credits)
           }
        |> take 10
    `);
    console.log("Query Results:", rows);

    // 9. DELETE Actor (Mode 0 = Strict bounded RESTRICT/CASCADE check)
    await client.delete(schema, "user-42", updated.actorRef, updated.version, { mode: 0 });
  } finally {
    await client.close();
    await identity.close();
  }
}

main().catch(console.error);
```

---

## 3. Go SDK Complete Workflow

```go
package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"os"
	"time"

	app "example.com/ciagomem-demo/generated/go/ciagomem/app"
	ciagomem "github.com/Ciago-Technologies/ciagomem-go"
	flatbuffers "github.com/google/flatbuffers/go"
)

func main() {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	token, _ := os.ReadFile(os.Getenv("CIAGOMEM_TOKEN_FILE"))
	ca, _ := os.ReadFile(os.Getenv("CIAGOMEM_CA_FILE"))
	roots := x509.NewCertPool()
	roots.AppendCertsFromPEM(ca)

	identity, err := ciagomem.OpenIdentityFile("go-client-state.bin")
	if err != nil {
		panic(err)
	}
	defer identity.Close()

	client, err := ciagomem.DialTLS(ctx, "db.example.internal:7443", &tls.Config{
		MinVersion: tls.VersionTLS13,
		ServerName: "db.example.internal",
		RootCAs:    roots,
	}, token, identity)
	if err != nil {
		panic(err)
	}
	defer client.Close()

	// 1. Register Schema
	bfbs, _ := os.ReadFile("build/app_schema.bfbs")
	schema, err := client.RegisterSchema(ctx, "users", 1, bfbs)
	if err != nil {
		panic(err)
	}

	// 2. Build FlatBuffer Document
	b := flatbuffers.NewBuilder(256)
	username := b.CreateString("anuj_tiwari")
	email := b.CreateString("anuj@ciagotech.com")
	admin := b.CreateString("admin")
	developer := b.CreateString("developer")

	app.UserProfileStartTagsVector(b, 2)
	b.PrependUOffsetT(developer)
	b.PrependUOffsetT(admin)
	tags := b.EndVector(2)

	app.UserProfileStart(b)
	app.UserProfileAddUsername(b, username)
	app.UserProfileAddEmail(b, email)
	app.UserProfileAddCredits(b, 100)
	app.UserProfileAddTags(b, tags)
	root := app.UserProfileEnd(b)
	b.FinishWithFileIdentifier(root, []byte("USER"))

	// 3. Create, Read, Mutate, Query, and Delete
	created, err := client.Create(ctx, schema, "user-42", b.FinishedBytes())
	if err != nil {
		panic(err)
	}

	doc, err := client.Read(ctx, schema, "user-42")
	if err != nil {
		panic(err)
	}
	user := app.GetRootAsUserProfile(doc.Bytes, 0)
	fmt.Printf("Read Credits: %d\n", user.Credits())

	updated, err := client.Mutate(ctx, schema, "user-42", created.ActorRef, doc.Version,
		`mutate {state.credits -= 25, state.tags.append("active_session")}`)
	if err != nil {
		panic(err)
	}

	rows, err := client.Query(ctx,
		`scan family("users") |> filter (state.credits > 50) |> aggregate {total_users: count()}`)
	if err != nil {
		panic(err)
	}
	fmt.Println("Aggregate Rows:", rows)

	_ = client.Delete(ctx, schema, "user-42", updated.ActorRef, updated.Version)
}
```

---

## 4. Rust Client SDK Complete Workflow

```rust
mod app_schema_generated;
use app_schema_generated::ciagomem::app::{UserProfile, UserProfileArgs};
use ciagomem_client::{Client, IdentityFile, Transport};
use std::{env, fs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = fs::read(env::var("CIAGOMEM_TOKEN_FILE")?)?;
    
    // Choose Transport::Tls for remote or Transport::UnixSeqpacket for local zero-copy memfd
    let transport = Transport::Tls {
        address: "db.example.internal:7443".into(),
        server_name: "db.example.internal".into(),
        ca_pem: fs::read(env::var("CIAGOMEM_CA_FILE")?)?,
    };

    let mut client = Client::connect(
        transport,
        token,
        IdentityFile::open("rust-client-state.bin")?,
    ).await?;

    let schema = client.register_schema(
        "users",
        1,
        &fs::read("build/app_schema.bfbs")?,
    ).await?;

    let mut builder = flatbuffers::FlatBufferBuilder::new();
    let username = builder.create_string("anuj_tiwari");
    let email = builder.create_string("anuj@ciagotech.com");
    let admin = builder.create_string("admin");
    let tags = builder.create_vector(&[admin]);

    let root = UserProfile::create(&mut builder, &UserProfileArgs {
        username: Some(username),
        email: Some(email),
        credits: 100,
        tags: Some(tags),
        memory_embeddings: None,
    });
    builder.finish(root, Some("USER"));

    let created = client.create(&schema, "user-42", builder.finished_data()).await?;
    let document = client.read(&schema, "user-42").await?;
    let profile = flatbuffers::root::<UserProfile<'_>>(&document.bytes)?;
    assert_eq!(profile.credits(), 100);

    let updated = client.mutate(
        &schema,
        "user-42",
        created.actor_ref,
        document.version,
        "mutate {state.credits -= 25, state.tags.append(\"active_session\")}",
    ).await?;

    let rows = client.query(
        "scan family(\"users\") |> filter (state.credits > 50) \
         |> group_by (state.username) |> aggregate {total_users: count()}",
    ).await?;
    println!("{rows:?}");

    client.delete(&schema, "user-42", updated.actor_ref, updated.version).await?;
    client.close().await?;
    Ok(())
}
```

---

## 5. CiaQL Language Reference & Advanced Patterns

### 5.1 Array & Bounded Ring-Buffer Mutations
Ciagomem supports high-speed array operations that append directly to the 8-byte-aligned delta tail without rewriting the base FlatBuffer:

| CiaQL Syntax | Operation | Behavior |
| :--- | :--- | :--- |
| `state.tags = ["a", "b"]` | `OpCode::SetBytes` | Replaces the entire vector after type and length validation. |
| `state.tags.append("vip")` | `OpCode::ArrayAppend` | Appends one element to the end of the vector. |
| `state.tags.remove(0)` | `OpCode::ArrayRemove` | Removes the element at zero-based index `0`, preserving order. |
| `state.tags.pop()` | `OpCode::ArrayPop` | Removes and returns the last element (`EmptyArray` if empty). |
| `state.tags.clear()` | `OpCode::ArrayClear` | Resets vector length to `0`. |
| `state.events.push_bounded("login", 100)` | `OpCode::ArrayPushBounded` | **Ring-Buffer Mode:** Appends `"login"` and evicts the oldest elements if length exceeds `100`. Returns discarded count. |
| `state.email = null` | `OpCode::SetScalar(null)` | Clears an optional/nullable field (does not delete the actor). |

### 5.2 Cross-Actor Joins (`JOIN`)
Actors can store versioned references (`ActorRef { id, incarnation }`) to other actors and resolve them across owner cores:

```text
@1
  |> join @actor(state.organization) as @org
  |> where (@org.state.name == "Ciago Technologies" and state.credits >= 100)
  |> select {
       user_email: state.email,
       org_name: @org.state.name,
       balance: state.credits
     }
```
*If `@org` was deleted and its ID was reused by a new actor, the 128-bit `incarnation` check fails closed rather than joining stale data.*

### 5.3 Referential Integrity (`LINK`) & Cascade Deletion Sagas
Register explicit dependency edges between actors within the same tenant:

```text
@2 link add(target: @1, on_delete: RESTRICT)
@3 link add(target: @1, on_delete: CASCADE)
```
* **Strict Deletion (`mode: 0`):** Validates the entire dependency closure before committing. If any `RESTRICT` edge points to the target outside the closure, the delete is rejected atomically.
* **Chunked Asynchronous Deletion Saga (`mode: 1`):** For massive fan-out closures, `begin_delete` journals the deletion plan and immediately tombstones all dependent actors (hiding them from scans and mutations), returning a `DeleteJob` ID for incremental background reclamation.

### 5.4 Hybrid AI Memory Search (BM25 Text + SQ8 SIMD Vector)
Combine exact metadata filtering, BM25 full-text scoring, and SIMD-quantized vector similarity in a single pipeline. Stages fuse rankings using **Reciprocal Rank Fusion (`1 / (60 + rank)`)**:

```text
scan family("agent_memories")
  |> filter (state.session_active == true)
  |> search(field: state.transcript, query: "cloud infrastructure migration", top_k: 50)
  |> similarity(vector: [0.12, -0.88, 0.44, 0.05], metric: COSINE, threshold: 0.80)
  |> select {
       summary: state.transcript,
       version: meta.schema_version
     }
  |> take 5
```

### 5.5 Lazy Schema Evolution (`ALTER SCHEMA`)
To evolve `users` from Version 1 to Version 2:
1. Append new optional fields with new `(id: N)` attributes in `app_schema.v2.fbs` (never change existing field types or reuse retired IDs; mark removed fields `deprecated`).
2. Compile `app_schema.v2.bfbs` and call `client.alterSchema("users", 2, bfbsBytes)`.
3. **Zero-Downtime Lazy Upcasting:** Existing Version 1 actors remain untouched on disk. When read, Ciagomem projects them into Version 2 with default values on the fly. When next mutated, the owner core compacts and upgrades the actor into a native Version 2 FlatBuffer.

---

## 6. Wire Protocol Status Codes Reference

All SDK methods map server responses to these canonical status codes:

| Code | Name | Description & Recommended Client Action |
| ---: | :--- | :--- |
| `0` | `OK` | Operation succeeded at the requested durability tier. |
| `1` | `AlreadyExists` | `CREATE` called on an already-live primary key. Use `UPSERT` or `MUTATE`. |
| `2` | `NotFound` | Target actor key does not exist or has been logically tombstoned. |
| `3` | `PermissionDenied` | Capability expired, wrong `BootEpoch`, or unauthorized family/CaMeL label. Renew token. |
| `4` | `SchemaMismatch` | Payload root or fingerprint does not match registered `SchemaCatalog` version. |
| `5` | `VersionConflict` | Optimistic concurrency failure (`expected_document_version` mismatch). Re-read and retry. |
| `7` | `DocumentTooLarge` | Compacted document exceeds the maximum 64 KiB slab class (`class 4`). |
| `8` | `Busy` | Per-actor checkpoint freeze barrier or ingress queue full. Back off and retry with same `client_seq`. |
| `9` | `IdempotencyConflict` | Retried `client_seq` was sent with a different payload digest. Bug in client caller. |
| `13` | `Indeterminate` | Connection dropped or storage fenced after submission. Retry with exact same `IdentityFile` state to learn outcome. |
| `17` | `ActorMoved` | Cluster placement epoch changed. SDK automatically updates route cache and retries target node. |
## Current runnable workflow

```sh
cargo run --release --offline -p ciagomem-engine --example quickstart
```

The SDK examples above are future M4–M5 acceptance workflows. They require SchemaCatalog, transport/auth/dedup and complete journal integration first. Conditional commands require the negotiated feature in specification 17.3. No package install or persistence guarantee follows from an example.
