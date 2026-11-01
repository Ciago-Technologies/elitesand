# Single-node TLS and retry verification

Current source fingerprints: source-sha256.json. Default release suite:235 reported passed; all-feature:263 reported passed. Each contains six explicit sandbox/kernel skips (native socket readiness, three older native I/O bodies, native WAL integration and real TCP listener). These are successful harness results, not235/263 executed native-hardware checks. The current network suite has12 cases and the durable retry suite8; durable file/restart checks remain included.

The same TLS/auth/framing/reactor executes on connected Unix streams for this sandbox. It verifies authenticated CRUD/CAS, dropped-ACK debit persistence across reopen, renewed boot auth, strict decoders, stream fragmentation/multiplexing, malformed CRC/oversize/duplicates, slow peers, bounded credits and protected client state. Native I/O and physical storage latency/durability are not inferred from these runs. Clippy passes with visible existing/new style and compatibility warnings; formatting, workspace/specification/documentation contracts and1,025 bounded mesh runs pass.

unit.log records an unavailable systemd-analyze verification: this sandbox denies SO_PASSCRED/SO_PASSRIGHTS setup (exit1). The unit was not installed/enabled and its host execution remains unverified. The CLI help and token-issuer tests executed. CI's pinned OpenSSL build is configured but not remotely executed here. Current provider is the locally installed OpenSSL3.5.9; its publisher SHA256 was fetched for the CI pin.

Earlier review/single-node logs/fingerprints describe the pre-network worker milestone. Current source replaces that milestone and is identified here. No Miri/fuzzing/ARM execution, power-cut/NVMe qualification, million-user capacity, nanosecond network latency or distributed correctness result is claimed.

The extracted ZIP also passed263 all-feature release tests with an empty Cargo home and a separate target directory, fully offline (package.log.gz). This uses the same installed system TLS dependency and preserves the six native/TCP skips.

Large raw logs are losslessly gzip-compressed; use gzip -dc to read them. log-sha256.json verifies the exact decompressed bytes for current and preceding worker-stage evidence. Short validator/diagnostic logs remain plain text.

CI checksum correction: the workflow pin accidentally contained65 hex digits (an extra leading1). The corrected64-digit digest matches both the publisher checksum file and the freshly downloaded50.8MiB archive. The exact sha256sum command passes; a malformed-pin regression check and workspace/documentation validators pass. A full local provider build reached Configure but stopped because this host lacks Perl FindBin.pm. Remote CI must confirm the complete provider build; no new Rust runtime changes or full-suite rerun are claimed for this checksum-only correction.
