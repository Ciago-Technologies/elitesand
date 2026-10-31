# Installation & Deployment Guide

CiagoMem — SLAE is distributed as a pre-built, production-hardened Linux container image and a suite of official language SDKs. No source code compilation is required to deploy or use the database.

---

## 1. Host Prerequisites (Linux Production Profile)

While CiagoMem can run in any standard Docker environment for development, production deployments achieving sub-microsecond read latencies and 50 µs group-commit durability require a Linux host with the following configuration:

* **Operating System:** Linux Kernel **5.10+** (Kernel **6.1+** recommended for optimal `io_uring` passthrough and `memfd` seal enforcement).
* **Storage:** Enterprise NVMe SSD with Power-Loss Protection (PLP) supporting `O_DIRECT`.
* **HugePages (`hugetlbfs`):** For a standard 16 GiB node running the 12 GiB resident arena profile, reserve at least 6,144 × 2 MiB HugePages on the host:
  ```bash
  # Reserve 6,144 x 2 MiB HugePages (12 GiB)
  sudo sysctl -w vm.nr_hugepages=6144

  # Verify HugePage allocation
  grep HugePages_ /proc/meminfo
  ```
* **File Descriptor & Memlock Limits:** Increase `nofile` and `memlock` limits so the engine can register `io_uring` fixed buffers and manage up to 16,384 concurrent `memfd` export windows.

---

## 2. Server Installation via Docker

### 2.1 Quickstart Development Container (`docker run`)
For local development, testing, and schema prototyping, start a single-node CiagoMem server using the official container image:

```bash
docker run -d \
  --name ciagomem-dev \
  -p 7443:7443 \
  -v $(pwd)/ciagomem-data:/var/lib/ciagomem \
  -v $(pwd)/certs:/etc/ciagomem/certs:ro \
  -e CIAGOMEM_MODE=development \
  -e CIAGOMEM_LOG_LEVEL=info \
  ciagotech/ciagomem:v0.3.0
```

### 2.2 Production Container (`docker run` with HugeTLB & `io_uring`)
For production workloads, mount `/dev/hugepages`, unlock `memlock` ulimits, and bind a local Unix Domain Socket directory (`/run/ciagomem`) if local zero-copy agents will run on the same host:

```bash
docker run -d \
  --name ciagomem-prod \
  --restart unless-stopped \
  -p 7443:7443 \
  --ulimit memlock=-1:-1 \
  --ulimit nofile=1048576:1048576 \
  --cap-add=IPC_LOCK \
  --cap-add=SYS_NICE \
  -v /dev/hugepages:/dev/hugepages \
  -v /var/lib/ciagomem:/var/lib/ciagomem \
  -v /run/ciagomem:/run/ciagomem \
  -v /etc/ciagomem/certs:/etc/ciagomem/certs:ro \
  -e CIAGOMEM_ARENA_GIB=12 \
  -e CIAGOMEM_REACTOR_CORES=14 \
  -e CIAGOMEM_TLS_CERT=/etc/ciagomem/certs/server.crt \
  -e CIAGOMEM_TLS_KEY=/etc/ciagomem/certs/server.key \
  -e CIAGOMEM_CA_CERT=/etc/ciagomem/certs/ca.crt \
  ciagotech/ciagomem:v0.3.0
```

---

## 3. Server Installation via Docker Compose

Create a `docker-compose.yml` file to manage your CiagoMem instance alongside your application services or CoveStack environment:

```yaml
version: "3.9"

services:
  ciagomem:
    image: ciagotech/ciagomem:v0.3.0
    container_name: ciagomem-slae
    restart: unless-stopped
    ports:
      - "7443:7443/tcp"   # Multiplexed TLS 1.3 Wire Protocol
      - "7443:7443/udp"   # Optional QUIC v1 Transport
    environment:
      - CIAGOMEM_NODE_ID=1
      - CIAGOMEM_REACTOR_CORES=8
      - CIAGOMEM_ARENA_GIB=10
      - CIAGOMEM_DEFAULT_DURABILITY=local-durable
      - CIAGOMEM_TLS_CERT=/etc/ciagomem/certs/server.crt
      - CIAGOMEM_TLS_KEY=/etc/ciagomem/certs/server.key
      - CIAGOMEM_CA_CERT=/etc/ciagomem/certs/ca.crt
      - CIAGOMEM_UDS_PATH=/run/ciagomem/db.sock
    ulimits:
      memlock:
        soft: -1
        hard: -1
      nofile:
        soft: 1048576
        hard: 1048576
    cap_add:
      - IPC_LOCK
      - SYS_NICE
    volumes:
      - ciagomem_wal:/var/lib/ciagomem/wal
      - ciagomem_extents:/var/lib/ciagomem/extents
      - ciagomem_backups:/var/backups/ciagomem
      - /dev/hugepages:/dev/hugepages
      - ./run/ciagomem:/run/ciagomem
      - ./certs:/etc/ciagomem/certs:ro
    healthcheck:
      test: ["CMD", "ciagomem-cli", "ping", "--socket", "/run/ciagomem/db.sock"]
      interval: 10s
      timeout: 3s
      retries: 3

volumes:
  ciagomem_wal:
    driver: local
  ciagomem_extents:
    driver: local
  ciagomem_backups:
    driver: local
```

Start the cluster using Docker Compose:
```bash
docker compose up -d
docker compose logs -f ciagomem
```

---

## 4. Installing the Schema Compiler (`flatc`)

CiagoMem requires **FlatBuffers `25.2.10`** to compile `.fbs` schema files into binary reflection descriptors (`.bfbs`) and strongly typed client code.

```bash
# Linux (x86_64)
curl -L [https://github.com/google/flatbuffers/releases/download/v25.2.10/Linux.flatc.binary.g++-13.zip](https://github.com/google/flatbuffers/releases/download/v25.2.10/Linux.flatc.binary.g++-13.zip) -o flatc.zip
unzip flatc.zip && chmod +x flatc && sudo mv flatc /usr/local/bin/

# Verify exact version (Must be 25.2.10)
flatc --version
```

---

## 5. Installing Language SDKs

Install the official CiagoMem client library and the FlatBuffers runtime for your application language:

### TypeScript / Node.js (Node 20+)
```bash
npm install @ciagomem/sdk flatbuffers@25.2.10
```

### Go (Go 1.22+)
```bash
go get [github.com/Ciago-Technologies/ciagomem-go@v0.3.0](https://github.com/Ciago-Technologies/ciagomem-go@v0.3.0)
go get [github.com/google/flatbuffers/go@v25.2.10+incompatible](https://github.com/google/flatbuffers/go@v25.2.10+incompatible)
```

### Rust (Rust 1.89+)
Add the following to your application's `Cargo.toml`:
```toml
[dependencies]
ciagomem-client = "0.3.0"
flatbuffers = "25.2.10"
tokio = { version = "1", features = ["full"] }
```

---

## 6. Key Operational Metrics to Monitor

When running in production, monitor the following per-core metrics exposed by the CiagoMem telemetry collector:

| Metric | Target / Alert Threshold | Description |
| :--- | :--- | :--- |
| `active_sessions_total` | `< 16,384` (for 64 MiB export budget) | Authenticated, non-expired logical sessions |
| `wal_free_bytes` | Alert if `< 20%` capacity | Remaining ring space in per-core WAL v2 |
| `oldest_pin_age_seconds` | Alert if `> 5s` | Age of oldest uncheckpointed mutation pin (triggers `FreezeCheckpoint`) |
| `generation_headroom_min` | Alert if `< 2^30` | Remaining 40-bit slot generations before proactive arena maintenance |
| `io_uring_cqe_latency_us` | Target `p99 ≤ 50 µs` | Submission-to-completion latency for NVMe group commits |
| `read_fallback_ratio` | Promotions trigger at `> 5%` | Ratio of seqlock reads hitting odd-writer contention |