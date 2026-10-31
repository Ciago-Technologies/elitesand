# Current Perplexity review prompt

Review the current five-crate workspace and master specification in the latest ZIP. Read docs/R9_REMEDIATION.md and docs/VERIFICATION.md first; historical patch/probe/report text is evidence, not current source.

Independently verify R9 teardown and publisher fixes plus earlier guarantees. Focus on pending I/O lifetime, late completions, release-then-resubmit, claim ownership, fence behavior and unsafe acknowledgement contracts. Inspect callers and distinguish tested components from missing node/replay/cancel-and-reap services. Run available positive and adversarial tests.

Report severity, file/line, reproduction, smallest compatible fix and acceptance test. Supply a focused patch ZIP and specification corrections. Attribute Miri/ARM results to the exact source tested, state unavailable checks, and make no unmeasured production or capacity claims.
