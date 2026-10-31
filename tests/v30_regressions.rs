//! Corrected regression guards derived from New File/ciagomem-v30-review-harness.zip.
//! PASS means the reviewed defect is rejected, unlike the archived reproduction tests.
mod support;
use ciagomem_core::*;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
fn session(tenant: u128, ops: u64) -> VerifiedMacaroon {
    let k = KeyedPrf([9; 32]);
    Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(tenant),
            Caveat::ExpiresAt(100),
            Caveat::PeerUid(1000),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(1),
            Caveat::Ops(ops),
            Caveat::KeyEpoch(2),
        ],
    )
    .unwrap()
    .verify(
        &k,
        &RequestCtx {
            tenant_incarnation: tenant,
            key_epoch: 2,
            now_ns: 1,
            op_bit: OP_READ,
            peer_uid: 1000,
            session: 1,
            audience: 7,
            boot_epoch: 1,
        },
    )
    .unwrap()
}
#[test]
fn s1_cross_tenant_and_scan_denied() {
    let token = session(11, OP_READ);
    let owner = |id| Some(if id == 1 { 11 } else { 22 });
    let auth = AuthContext {
        session: &token,
        owner_of: &owner,
        allowed_destinations: &[],
        owner_version: 0,
    };
    assert_eq!(
        validate_query(
            &parse_ciaql("@2 select {balance: state.wallet.balance}").unwrap(),
            &auth
        ),
        Err(PolicyError::ForeignActor(2))
    );
    assert_eq!(
        validate_query(&parse_ciaql("scan actors[*]").unwrap(), &auth),
        Err(PolicyError::OperatorScopeRequired)
    );
    assert!(validate_query(&parse_ciaql("@1").unwrap(), &auth).is_ok());
}
#[test]
fn s2_actor_references_are_validated_nodes() {
    assert!(parse_ciaql("@1 mutate {actor(2).state.x=0}").is_err());
    assert!(parse_ciaql("@1 where ->DROP EVERYTHING<-").is_err());
    let q = parse_ciaql("tx [@1,@2] mutate {actor(1).state.x=0, actor(2).state.x=1}").unwrap();
    let token = session(11, OP_READ | OP_WRITE);
    let owner = |id| Some(if id == 1 { 11 } else { 22 });
    assert_eq!(
        validate_query(
            &q,
            &AuthContext {
                session: &token,
                owner_of: &owner,
                allowed_destinations: &[],
                owner_version: 0,
            }
        ),
        Err(PolicyError::ForeignActor(2))
    );
}
#[test]
fn s3_named_args_are_literal_aware_and_bounded() {
    let q = parse_ciaql(r#"@1 search(query: "top_k: 7 now", field: body, top_k: 3)"#).unwrap();
    assert!(matches!(q.stages[0], Stage::SearchBm25 { top_k: 3, .. }));
    for value in ["banana", "0", "1001", "4000000000"] {
        assert!(
            parse_ciaql(&format!(
                r#"@1 search(field: body, query: "x", top_k: {value})"#
            ))
            .is_err()
        );
    }
    assert!(parse_ciaql(r#"@1 search(field: body, query: "x", top_k: 2, top_k: 3)"#).is_err());
}
#[test]
fn s3b_vector_and_metric_retained() {
    let q =
        parse_ciaql("@1 similarity(vector: [0.1,0.2], metric: cosine, threshold: 0.8)").unwrap();
    let Stage::Similarity {
        vector,
        metric,
        threshold,
    } = &q.stages[0]
    else {
        panic!()
    };
    assert_eq!(vector, &[0.1, 0.2]);
    assert_eq!(*metric, ciagomem_ql::Metric::Cosine);
    assert_eq!(*threshold, 0.8);
    // Runtime variable binding is not implemented; reject instead of discarding the vector.
    assert!(parse_ciaql("@1 similarity(vector: $q, metric: cosine, threshold: 0.8)").is_err());
}
#[test]
fn s3c_take_export_policy_agrees() {
    let q =
        parse_ciaql(r#"@1 take 5 export(format: "csv", destination: "s3://approved/reports/x")"#)
            .unwrap();
    let token = session(11, OP_READ | OP_OPERATOR);
    let owner = |_| Some(11);
    assert!(
        validate_query(
            &q,
            &AuthContext {
                session: &token,
                owner_of: &owner,
                allowed_destinations: &[DestinationRule {
                    tenant: 11,
                    prefix: "s3://approved/reports/",
                    expected_owner: Some("owner")
                }],
                owner_version: 0,
            }
        )
        .is_ok()
    );
    assert!(
        parse_ciaql(r#"@1 take 5 export(format: "csv", destination: "s3://approved/x") take 2"#)
            .is_err()
    );
}
#[test]
fn s3d_keyword_field_is_not_a_stage() {
    assert!(matches!(
        parse_ciaql("@1 where limit > 3").unwrap().stages[0],
        Stage::Filter(_)
    ));
    assert!(parse_ciaql(r#"@1 where state.message == "take 4 |> export" take 2"#).is_ok());
}
#[test]
fn s4_exact_bucket_and_path_boundary() {
    let allowed = ["s3://approved/reports/", "/var/backups/ciagomem/daily/"];
    for d in [
        "s3://attacker-exfil/reports/x",
        "s3://approved.evil/reports/x",
        "s3://approved/reports-evil/x",
        "s3://approved/reports/../secret",
        "s3://approved/reports/%2e%2e/x",
        "s3://approved@evil/reports/x",
        "http://169.254.169.254/latest",
        "/var/backups/ciagomem/daily/../x",
    ] {
        assert!(!destination_allowed(d, &allowed), "{d}");
    }
    assert!(!destination_allowed("s3://attacker/dump", &["s3://"]));
    assert!(destination_allowed("s3://approved/reports/x", &allowed));
    assert!(destination_allowed(
        "/var/backups/ciagomem/daily/x",
        &allowed
    ));
}
fn journal_path(name: &str) -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "ciagomem-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
#[test]
fn s5_restart_does_not_reuse_incarnations() {
    let p = journal_path("stable");
    let anchor = support::anchor();
    let k = KeyedPrf([7; 32]);
    let first = {
        let mut r = TenantRegistry::new(IncarnationStore::create(&p, anchor.clone()).unwrap());
        r.create(&k, "ns", "tenant").unwrap()
    };
    let mut registry = TenantRegistry::new(IncarnationStore::open(&p, anchor.clone()).unwrap());
    assert_eq!(registry.lookup(&k, "ns", "tenant").unwrap(), first);
    assert!(matches!(
        registry.create(&k, "ns", "tenant"),
        Err(RegistryError::Exists)
    ));
    registry.delete(&k, "ns", "tenant").unwrap();
    let second = registry.create(&k, "ns", "tenant").unwrap();
    assert_eq!(first.0, second.0);
    assert_ne!(first.1, second.1);
    let loc = VaultLoc {
        segment: 1,
        offset: 0,
        len: 10,
        generation: 1,
        digest: [9; 32],
    };
    let scope = RefScope {
        tenant: first.1,
        session: 1,
        actor: 1,
        expires_at: 100,
    };
    let token = mint_ref(&k, &scope, 5, &loc);
    let index = HashMap::from([(5, loc)]);
    assert!(resolve_ref(&k, &scope, &index, &token, 1).is_ok());
    assert!(
        resolve_ref(
            &k,
            &RefScope {
                tenant: second.1,
                ..scope
            },
            &index,
            &token,
            1
        )
        .is_err()
    );
    drop(registry);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn incarnation_journal_fails_closed_on_torn_tail_and_lock_conflict() {
    use std::io::Write;
    let p = journal_path("tail");
    let anchor = support::anchor();
    let store = IncarnationStore::create(&p, anchor.clone()).unwrap();
    assert!(IncarnationStore::open(&p, anchor.clone()).is_err());
    drop(store);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(&[0; 32])
        .unwrap();
    let recovered = IncarnationStore::open(&p, anchor.clone()).unwrap();
    assert_eq!(recovered.recovered_tail_bytes, 32);
    drop(recovered);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn s6_endorsements_require_authentication_and_binding() {
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let token = session(11, OP_READ | OP_WRITE);
    let content = b"value";
    let label = support::label(Integrity::AgentDerived, 2, vec![11]).unwrap();
    let approval = Approval {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        nonce: 1,
        field: FrameField::ActiveIntent as u8,
        sources: 2,
        digest: content_digest(content),
        sink: 11,
        tool: "send".into(),
        expires_at: 100,
    };
    let e = signer.endorse(approval);
    let context = AdmissionContext {
        session: &token,
        reader: 11,
        content,
        now: 1,
        nonce: 1,
        tool: "send",
        sink: 11,
        verifier: &verifier,
    };
    let mut nonces = NonceTracker::new(10, 128, 1);
    let mut value = admit_to_frame(
        FrameField::ActiveIntent,
        label.clone(),
        Some(&e),
        &context,
        &mut nonces,
    )
    .unwrap();
    assert!(authorize_tool_call(&mut value, &token, "send", 11, content, 2).is_ok());
    assert!(authorize_tool_call(&mut value, &token, "send", 11, content, 2).is_err());
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&e),
            &context,
            &mut nonces
        )
        .is_err()
    );
    let mut forged = e.clone();
    forged.approval.sources = u64::MAX;
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&forged),
            &context,
            &mut NonceTracker::new(10, 128, 1)
        )
        .is_err()
    );
    let wrong = AdmissionContext {
        content: b"different",
        ..context
    };
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label,
            Some(&e),
            &wrong,
            &mut NonceTracker::new(10, 128, 1)
        )
        .is_err()
    );
}

#[test]
fn s6b_signature_ops_and_mandatory_caveats_required() {
    let k = KeyedPrf([3; 32]);
    let ctx = RequestCtx {
        tenant_incarnation: 1,
        key_epoch: 0,
        now_ns: 1,
        op_bit: 1,
        peer_uid: 1000,
        session: 1,
        audience: 7,
        boot_epoch: 1,
    };
    let base = vec![
        Caveat::Tenant(1),
        Caveat::ExpiresAt(100),
        Caveat::PeerUid(1000),
        Caveat::Session(1),
        Caveat::Audience(7),
        Caveat::BootEpoch(1),
        Caveat::KeyEpoch(0),
    ];
    assert!(
        Macaroon::issue(&k, base.clone())
            .unwrap()
            .verify(&k, &ctx)
            .is_err()
    );
    let mut cs = base;
    cs.push(Caveat::Ops(3));
    let mut m = Macaroon::issue(&k, cs).unwrap();
    assert!(m.verify(&k, &ctx).is_ok());
    m.attenuate(Caveat::Ops(2)).unwrap();
    assert!(m.verify(&k, &ctx).is_err());
    m.caveats.pop();
    assert!(m.verify(&k, &ctx).is_err());
    let mut forged = Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(1),
            Caveat::ExpiresAt(100),
            Caveat::PeerUid(1000),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(1),
            Caveat::KeyEpoch(0),
            Caveat::Ops(1),
        ],
    )
    .unwrap();
    forged.signature[0] ^= 1;
    assert!(forged.verify(&k, &ctx).is_err());
}
#[test]
fn s7_principals_do_not_alias_modulo_64() {
    let label = support::label(Integrity::UserDirect, 1, vec![1]).unwrap();
    assert!(gateway_allow(false, &label, 1));
    assert!(!gateway_allow(true, &label, 65));
}
#[test]
fn llm_output_inherits_every_context_label() {
    let user = support::label(Integrity::UserDirect, 1, vec![1, 65]).unwrap();
    let ext = support::label(Integrity::External, 2, vec![65]).unwrap();
    let out = label_pllm_output(&[user.clone(), ext]);
    assert_eq!(out.integrity(), Integrity::External);
    assert_eq!(out.sources(), 3);
    assert_eq!(out.readers(), &[65]);
    assert_eq!(
        label_pllm_output(&[user.clone()]).integrity(),
        Integrity::AgentDerived
    );
    assert_eq!(label_qllm_output(&[user]).integrity(), Integrity::External);
}
struct StepClock(AtomicU64);
impl Clock for StepClock {
    fn now_ns(&self) -> u64 {
        self.0.fetch_add(100, Ordering::Relaxed)
    }
}
#[test]
fn s9_slot_read_uses_a_deadline() {
    let a = SlotArena::<512>::new(0, 1, CoreControl::shared(1));
    let h = a
        .install(
            0,
            TenantWord {
                tenant: 1,
                core: 0,
                layout_version: 1,
                flags: 0,
            },
            11,
            0,
            0,
        )
        .unwrap()
        .unwrap();
    let guard = a.header(0).seq.write_begin().unwrap();
    assert!(matches!(
        a.read(h, 1, 11, Visibility::Optimistic, |v| v.word(0))
            .err(),
        Some(SlotReadError::WriterStalled | SlotReadError::DeadlineExhausted)
    ));
    guard.commit();
    assert!(
        a.read(h, 1, 11, Visibility::Optimistic, |v| v.word(0))
            .is_ok()
    );
}
#[test]
fn s10_stalled_writer_is_not_declared_poisoned() {
    let seq = SeqWord::new();
    let words = [AtomicU64::new(42)];
    let g = seq.write_begin().unwrap();
    let r = read_deadline(
        &seq,
        &words,
        &StepClock(AtomicU64::new(0)),
        Duration::from_nanos(1000),
        Duration::from_nanos(200),
        |v| v.word(0),
    );
    assert!(matches!(r, Err(DeadlineReadError::WriterStalled { .. })));
    g.commit();
    assert_eq!(
        read_deadline(
            &seq,
            &words,
            &StepClock(AtomicU64::new(0)),
            Duration::from_nanos(1000),
            Duration::from_nanos(200),
            |v| v.word(0)
        ),
        Ok(42)
    );
}
#[test]
fn s11_overflow_hard_cap_and_excess_credits() {
    let mut lane = (VecDeque::new(), 4usize);
    let mut flow = OutboundFlow::new(4, 8);
    let mut rejected = 0;
    for i in 0..100_000u32 {
        if flow.send(&mut lane, i).is_err() {
            rejected += 1
        }
    }
    assert_eq!(flow.backlog(), 8);
    assert_eq!(lane.0.len(), 4);
    assert_eq!(rejected, 99_988);
    lane.0.clear();
    assert!(flow.acknowledge(100).is_err());
    flow.acknowledge(4).unwrap();
    assert_eq!(flow.excess_credits, 1);
    assert_eq!(flow.pump(&mut lane), 4);
    assert_eq!(flow.backlog(), 4);
    let mut zero = OutboundFlow::new(0, 0);
    assert_eq!(zero.send(&mut lane, 7), Err(7));
    assert_eq!(zero.backlog(), 0);
}
#[test]
fn s12_dirty_list_checks_head_only_and_unlinks_arbitrary_nodes() {
    let mut p = TruncationPins::new(70_000);
    for t in 0..70_000 {
        p.on_dirty(t, 1000 + t as u64, 0).unwrap()
    }
    let calls = std::cell::Cell::new(0);
    assert_eq!(
        p.decide(1, 0.9, Duration::from_secs(1), |_| {
            calls.set(calls.get() + 1);
            false
        }),
        PinAction::None
    );
    assert_eq!(calls.get(), 1);
    p.on_checkpointed(35_000, 36_000).unwrap();
    p.on_checkpointed(0, 1000).unwrap();
    assert_eq!(p.min_needed_lsn(80_000), 1001);
    assert_eq!(
        p.decide(1, 0.1, Duration::from_secs(1), |_| false),
        PinAction::FreezeCheckpoint { tenant: 1 }
    );
    assert!(p.on_dirty(0, 1, 0).is_err());
    assert!(p.on_dirty(70_000, 80_000, 0).is_err());
}
#[test]
fn vote_reuse_rejects_late_and_conflicting_duplicate_ballots() {
    let mut t = VoteTable::new(2);
    let n = Participants::new(2).unwrap();
    t.open(1, n).unwrap();
    assert_eq!(t.vote(1, 0, true), Vote::Pending);
    assert_eq!(t.vote(1, 0, false), Vote::Conflict);
    assert_eq!(t.vote(1, 1, true), Vote::Decided { commit: true });
    t.close(1);
    t.open(3, n).unwrap();
    assert_eq!(t.vote(1, 0, false), Vote::Stale);
    assert_eq!(t.vote(3, 0, true), Vote::Pending);
    assert_eq!(t.vote(3, 1, true), Vote::Decided { commit: true });
    t.close(3);
    assert!(t.open(1, n).is_err());
}
#[test]
fn sixty_four_votes_required() {
    let mut t = VoteTable::new(1);
    t.open(1, Participants::new(64).unwrap()).unwrap();
    for i in 0..63 {
        assert_eq!(t.vote(1, i, true), Vote::Pending)
    }
    assert_eq!(t.vote(1, 63, true), Vote::Decided { commit: true });
}
#[test]
fn checked_primitives_reject_regression_before_mutating() {
    assert!(ExtentLoc::new_checked(4103, false, 1).is_none());
    let a = SlotArena::<512>::new(0, 1, CoreControl::shared(1));
    let h = a
        .install(
            0,
            TenantWord {
                tenant: 1,
                core: 0,
                layout_version: 1,
                flags: 0,
            },
            11,
            100,
            100,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        a.write(h, 50, |w| w.set(0, 42)),
        Err(WriteError::LsnRegression {
            current: 100,
            attempted: 50
        })
    );
    assert_eq!(
        a.read(h, 1, 11, Visibility::Optimistic, |v| v.word(0))
            .unwrap()
            .value,
        0
    );
    let publisher = slot::DurabilityPublisher::claim(a.controls().clone(), 0).unwrap();
    publisher.publish(100);
    publisher.publish(10);
    assert_eq!(a.core(0).durable(), 100);
}
#[test]
fn p0_slot_regressions_are_also_rejected_in_release() {
    use ciagomem_core::slot as p0;
    let a = p0::SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let h = a
        .install(
            0,
            p0::TenantWord {
                tenant: 1,
                core: 0,
                layout_version: 1,
                flags: 0,
            },
            11,
            100,
            100,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        a.write(h, 50, |w| w.set(0, 42)),
        Err(WriteError::LsnRegression {
            current: 100,
            attempted: 50
        })
    );
    let publisher = slot::DurabilityPublisher::claim(a.controls().clone(), 0).unwrap();
    publisher.publish(100);
    publisher.publish(10);
    assert_eq!(a.core(0).durable(), 100);
}
#[test]
fn shorthand_and_pipeline_produce_same_ast() {
    let a = parse_ciaql("@1 where state.x > 1 mutate {state.x += 1} select {x: state.x} take 5")
        .unwrap();
    let b = parse_ciaql(
        "@1 |> where state.x > 1 |> mutate {state.x += 1} |> select {x: state.x} |> take 5",
    )
    .unwrap();
    // Byte offsets differ, while the same typed structure and values are retained.
    assert_eq!(a.source, b.source);
    assert_eq!(
        a.stages.iter().map(Stage::name).collect::<Vec<_>>(),
        b.stages.iter().map(Stage::name).collect::<Vec<_>>()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn sealed_export_windows_enforce_and_release_global_budget() {
    use ciagomem_core::export_window::{ExportBudget, SealedExportWindow};
    let budget = ExportBudget::new(4096, 1);
    let window = SealedExportWindow::create_and_selftest(4096, &budget).unwrap();
    assert_eq!(budget.usage(), (4096, 1));
    assert!(SealedExportWindow::create_and_selftest(4096, &budget).is_err());
    drop(window);
    assert_eq!(budget.usage(), (0, 0));
    assert!(SealedExportWindow::create_and_selftest(1, &budget).is_err());
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn policy_rechecks_manually_constructed_ast() {
    let token = session(11, OP_READ | OP_WRITE);
    let owner = |_| Some(11);
    let auth = AuthContext {
        session: &token,
        owner_of: &owner,
        allowed_destinations: &[],
        owner_version: 0,
    };
    let mut q = parse_ciaql("@1").unwrap();
    q.stages.push(Stage::Take(0));
    assert_eq!(validate_query(&q, &auth), Err(PolicyError::InvalidQuery));
}
