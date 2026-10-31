# Gemini with Argon review prompt

Audit the attached R11 workspace and both specifications. Read docs/R11_REMEDIATION.md and docs/VERIFICATION.md. Verify CPU/DMA exclusion, range claims, unwind/teardown, pin saturation and single-use releases; preserve R5–R10 invariants. Run default/all-feature release tests and specification checks. Distinguish implemented components from planned schema/SDK/cluster features. Report only reproducible defects with severity, file/line, minimal fix and regression test; state skipped checks. Do not infer performance or production readiness from passing tests.
