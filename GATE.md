# Elitesand — SLAE: Production Gates

**Version:** 0.3.0
**Status:** Audit complete — all gates are **documentation only**; no code changes required now.
**Purpose:** Checklist to satisfy before any production deployment claim.

---

## Gate 0: Baseline Verification (Already Passing)

| Check                                   | Command                                      | Status  |
| --------------------------------------- | -------------------------------------------- | ------- |
| Unit + regression tests                 | `cargo test --release --offline --workspace` | ✅ PASS |
| Formatting                              | `cargo fmt --all -- --check`                 | ✅ PASS |
| Workspace validation                    | `python3 scripts/validate_workspace.py`      | ✅ PASS |
| Documentation validation                | `python3 scripts/validate_documentation.py`  | ✅ PASS |
| Compile-fail doc tests (API invariants) | Included in test suite                       | ✅ PASS |

**Gate 0 is the minimum CI gate.** Every PR must pass these.

---

## Gate 1: Dependency & Supply Chain

| Requirement                                                                     | Verification                                                                    | Notes                  |
| ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- | ---------------------- |
| All `vendor/` crates pinned at exact versions in `Cargo.toml` [patch.crates-io] | `cargo tree -d` shows no duplicates                                             | ✅ Already done        |
| `legacy-audit` feature **disabled** in release builds                           | `cargo build --release --offline 2>&1 \| grep -q "legacy-audit" \|\| echo "OK"` | Add to CI              |
| No unpatched CVEs in transitive deps                                            | `cargo audit` / `cargo deny check` in CI                                        | Run before release tag |
| `flatbuffers` + `io-uring` versions frozen                                      | `Cargo.lock` committed                                                          | ✅ Already done        |

**Action:** Add `cargo deny check` and `legacy-audit` absence check to CI pipeline.

---

## Gate 2: Hardware & Kernel Prerequisites (Pre-Flight)

The single-node TLS service (`elitesand-server`) **requires** these on the target host:

| Resource            | Minimum                     | Verification Command                      |
| ------------------- | --------------------------- | ----------------------------------------- |
| Linux kernel        | ≥ 5.10 (io_uring stable)    | `uname -r`                                |
| `io_uring` support  | `CONFIG_IO_URING=y`         | `grep IO_URING /boot/config-$(uname -r)`  |
| HugeTLB 2 MiB pages | ≥ 2 GiB reserved            | `grep HugePages_Total /proc/meminfo`      |
| Dedicated CPUs      | 2 isolated cores            | `cat /sys/devices/system/cpu/isolated`    |
| NVMe with FLUSH/FUA | AWUPF ≥ 4 KiB               | `nvme id-ctrl /dev/nvme0n1 \| grep AWUPF` |
| OpenSSL             | ≥ 3.5.9 (LTS) + dev headers | `openssl version`                         |
| File permissions    | Data dir 0700, keys 0600    | `stat -c '%a' /path`                      |

**Action:** Create a `preflight-check` binary (or script) that validates all above and exits non-zero on failure. Run before `elitesand-server` systemd unit starts.

---

## Gate 3: Capacity & Performance Qualification

**Spec claims** (MASTER_SPECIFICATION.md §1, README §15):

- 1,000,000 registered tenants / 16 GiB node
- 200 ns local read (excludes network, auth, query, persistence)
- 15-modality workload map

**Required before claiming capacity:**

| Benchmark                                           | Target Hardware       | Pass Criteria                                               |
| --------------------------------------------------- | --------------------- | ----------------------------------------------------------- |
| `cargo bench -p elitesand-engine pinned_actor_alpha` | Production NVMe + CPU | Documented p50/p99 latency, throughput                      |
| Sustained write + checkpoint cycle                  | Same                  | No WAL stall > 10 ms, no OOM                                |
| 1M tenant residency simulation                      | Same                  | Memory ≤ 16 GiB, no latency degradation                     |
| Power-loss recovery (simulated)                     | Same                  | `SimIo::power_loss` test passes, 0 acknowledged writes lost |

**Action:** Run benchmarks on **exact production hardware** before any capacity SLA. Record results in `BENCHMARKS.md`.

---

## Gate 4: Durability & Power-Loss Qualification

| Test                                | Tool                                             | Required Evidence                              |
| ----------------------------------- | ------------------------------------------------ | ---------------------------------------------- |
| WAL crash recovery (all tail modes) | `cargo test --release --offline -p elitesand-wal` | 0 data loss across 1000+ simulated power cuts  |
| Checkpoint A/B flip atomicity       | Same                                             | Torn checkpoint never destroys last good image |
| Drive write cache behavior          | `nvme flush`, `hdparm -F`                        | FLUSH/FUA honored; no silent data loss         |
| Filesystem + drive combo            | `fio` with `sync=1`, `direct=1`                  | No corruption after hard power off             |

**Action:** Qualify **each NVMe model + filesystem** (ext4/xfs/btrfs) used in production. Document in `DURABILITY.md`.

---

## Gate 5: Security & Cryptographic Hygiene

| Requirement                                             | Verification                                              |
| ------------------------------------------------------- | --------------------------------------------------------- |
| Ed25519 keys generated from OS entropy (`/dev/urandom`) | `elitesand-crypto::random_id()` — already implemented      |
| SessionEpoch unique per process start                   | `SessionEpoch::new()` — already implemented               |
| Macaroon caveats all mandatory (9 caveats)              | `VerifiedMacaroon::verify` enforces — already implemented |
| Nonce tracker prevents replay                           | `NonceTracker::consume` — already implemented             |
| TLS 1.3 only, mTLS enforced                             | `TlsContext::new` with CA — already implemented           |
| No `legacy-audit` code in release binary                | Gate 1 check                                              |

**Action:** Rotate root keys annually. Document key ceremony in `SECURITY_OPS.md`.

---

## Gate 6: Operational Runbooks (Must Exist Before Deploy)

| Runbook               | Contents                                                                     |
| --------------------- | ---------------------------------------------------------------------------- |
| `INSTALL.md` (extend) | Pre-flight check, systemd unit, log rotation, backup schedule                |
| `DURABILITY.md`       | Drive qualification results, recovery procedures, RPO/RTO                    |
| `BENCHMARKS.md`       | Hardware, config, results, date, git SHA                                     |
| `SECURITY_OPS.md`     | Key rotation, cert renewal, incident response                                |
| `UPGRADE.md`          | Schema migration, WAL format compatibility, rollback plan                    |
| `MONITORING.md`       | Key metrics (durable_lsn, flush latency, free slots, pin saturation), alerts |

**Action:** Write these before first production deploy. They are **not code** — they are operational contracts.

---

## Gate 7: Distributed / Multi-Node (Explicitly Out of Scope)

The following are **target work** (MASTER_SPECIFICATION.md §14–15) and **not required** for single-node deployment:

- BFBS schema families & general document arrays
- Go / TypeScript / Rust SDK publications
- Cluster replication, gateway routing, query workers
- Columnar shadow store, analytical worker isolation
- Fleet/S3 backup adapters, repair integration
- Reactor control-priority dispatch, cancel/reap driver

**Do not block single-node release on these.** Track in separate milestone.

---

## Gate Summary Checklist (Copy-Paste for Release PR)

```
[ ] Gate 0: All CI checks pass (tests, fmt, validate scripts)
[ ] Gate 1: cargo deny clean; legacy-audit absent from release build
[ ] Gate 2: Pre-flight check passes on target host
[ ] Gate 3: Benchmarks run on prod hardware; results in BENCHMARKS.md
[ ] Gate 4: Drive + FS qualified; results in DURABILITY.md
[ ] Gate 5: Key rotation procedure documented in SECURITY_OPS.md
[ ] Gate 6: All 6 runbooks exist and reviewed
[ ] Gate 7: Distributed scope acknowledged as future work
```

---

## Maintenance

- Review gates quarterly or before any major version bump
- Add new gates only via PR with justification
- Gates are **process**, not code — they live here, not in the compiler
