# Memory orchestration: research and implementation decisions

Research completed before runtime implementation, 1 November 2026. This is an engineering threat assessment, not an independent penetration test or a claim of complete prompt-injection resistance.

## Primary evidence

- [Anthropic: multi-agent research systems](https://www.anthropic.com/research/multi-agent-research-system),13 June2025: compression through scoped workers helps lead-agent context, but their observed multi-agent token consumption was substantially higher than chat. This is not a measurement of Ciagomem or proof of net savings.
- [Anthropic: context engineering](https://www.anthropic.com/research/effective-context-engineering-for-ai-agents),29 September2025: compact notes, selective retrieval and focused subagents are complementary context-management approaches. Use a hybrid rather than recursively delegating every lookup.
- [CaMeL original paper](https://arxiv.org/abs/2503.18813),v2 June2025: capabilities and control/data-flow enforcement are the security boundary. A summary cannot turn an untrusted instruction into trusted control merely by passing through another model.
- [OpenAI: designing agents to resist prompt injection](https://openai.com/index/designing-agents-to-resist-prompt-injection/),11 March2026: constrain dangerous source-to-sink flows even if the model is manipulated. Prompt filtering alone does not establish containment.

## Risks and required controls

| Risk in the proposed example | Chosen control |
| --- | --- |
| Summary launders External logs into trusted AgentDerived facts | Existing Label::derived preserves minimum parent integrity and reader intersection; source taint remains External. Never automatically grant Policy/UserDirect or a FramePermit |
| Worker edits unrelated user state | Broker privately owns its Engine; worker APIs accept an opaque lease and verified worker session for exact source/output scope. Namespace/family capabilities remain future host work |
| Cross-tenant/session replay or fabricated handoff | Bind broker random ID, full actor references, controller/worker session, tenant, audience, boot and bounded server-clock deadline; private permit/reference fields |
| Raw log changes while summary is computed | Capture full image digests once; private-engine revision invalidation rejects source updates/deletions before publication/retrieval. Avoid rehashing large sources on each main-agent read. External files require a trusted adapter to report changes. |
| Multiple workers overwrite one output | Exclusive task output reservation and publication state; same-content retry returns existing reference, conflicting retry rejects |
| Prompt/token/cost runaway | Bound sources, source-image bytes, read attempts, pending jobs, summary bytes, context bytes/items and lease/summary lifetimes. Provider calls/tokens/cost quotas remain host-enforced |
| Untrusted path/URL or CiaQL interpolation triggers effects | Runtime broker never opens files, executes tools, fetches URLs, invokes models or evaluates summary text. Publication accepts data strings, not generated mutation commands |
| False or lossy summary guides high-impact decisions | Retain source citations/digests; on uncertainty escalate to explicitly budgeted evidence inspection. Summary is evidence, not an instruction or truth proof |
| Duplicate journal effect after retry | Update broker state only after Engine journal success; same live-task digest retry is idempotent. Full persisted task/dedup recovery remains planned |
| Tenant DoS or retained context | One broker binds one tenant/controller, has hard task limits and explicit forget. Engine and journal quotas remain authoritative; host must cap broker instances and caller-held snapshots |
| False zero-copy/zero-token economics | Inference still tokenizes text; actual worker/main/tool/cache/retry usage and quality must be measured. MemoryJournal is not durable; current broker allocates bounded owned data |

## Selected implementation

Add a bounded in-process MemoryBroker over the existing Engine, actor FlatBuffer images, authorization clock, labels and journal. Keep the audited seqlock/header/WAL/export invariants intact. Add adversarial tests for scope, TTL/rollback, freshness, output collisions, provenance, limits, duplicate publication and journal failure. Add a runnable provider-neutral worker handoff example; do not add provider SDKs or new dependencies.

Target integration retains sections14–17's collections, typed delta arrays, BM25/SQ8 retrieval, local exports, crash-safe client ledger and durable host as separate milestones. This change is not an implementation of all those services. Do not run an untrusted worker with direct Engine access, an issuer signing key or arbitrary filesystem/network tools.

Trust assumption: the model is untrusted, but its tool adapter and issuer verification are trusted. VerifiedMacaroon values must be obtained from the configured trusted verifier; native plugins able to mint/verify with their own keys are outside this in-process boundary. Expose model-facing wrappers bound to host-held sessions/leases rather than accepting native credential objects from a worker.
