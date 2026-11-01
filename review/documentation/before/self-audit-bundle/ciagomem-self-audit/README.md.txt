# Reproduce the R10 self-audit

Extract `ciagomem-self-audit/` alongside the audited `ciagomem-slae/` directory. The probe uses the repository's vendored dependencies. From the parent directory run:

```sh
CARGO_TARGET_DIR=/tmp/ciagomem-self-audit-target cargo test --release --offline --manifest-path ciagomem-self-audit/probe/Cargo.toml
```

These three tests PASS WHEN THE DEFECTS EXIST. They are deliberately failing-behavior demonstrations, not fixed-code regression expectations. Read the report for the acceptance tests needed after remediation. They use simulated I/O and genuine simulator completions. They do not execute native DMA.

The full source is in the separate R10 review bundle or your working tree; this archive contains evidence only. Compare the hashes in source-sha256.json before reproduction.
