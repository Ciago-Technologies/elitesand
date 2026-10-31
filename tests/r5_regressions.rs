//! Corrected guards: passing means the R5 defect is prevented.
mod support;
use ciagomem_core::*;
use std::{collections::VecDeque, fs, io::Write};
fn session(tenant: u128) -> VerifiedMacaroon {
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
            Caveat::Ops(OP_READ | OP_OPERATOR),
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
fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ciagomem-r5-{}-{name}", std::process::id()))
}
#[test]
fn d1_checkpoint_cannot_drop_a_newer_mutation_pin() {
    let mut p = TruncationPins::new(2);
    p.on_dirty(0, 10, 0).unwrap();
    p.on_dirty(0, 20, 1).unwrap();
    assert_eq!(
        p.on_checkpointed(0, 10),
        Err(dirty::DirtyError::StaleCheckpoint)
    );
    assert_eq!(p.min_needed_lsn(99), 10);
    p.on_checkpointed(0, 20).unwrap();
    assert_eq!(p.min_needed_lsn(99), 99);
}
#[test]
fn t1_t2_t3_persistent_identity_rejects_rollback_and_acknowledged_corruption() {
    let path = temp("identity");
    let _ = fs::remove_file(&path);
    let anchor = support::anchor();
    let k = KeyedPrf([9; 32]);
    let mut r = TenantRegistry::new(IncarnationStore::create(&path, anchor.clone()).unwrap());
    let original = r.create(&k, "ns", "name").unwrap();
    drop(r);
    let old = fs::read(&path).unwrap();
    let mut r = TenantRegistry::new(IncarnationStore::open(&path, anchor.clone()).unwrap());
    assert_eq!(r.lookup(&k, "ns", "name").unwrap(), original);
    assert!(matches!(
        r.create(&k, "ns", "name"),
        Err(RegistryError::Exists)
    ));
    r.delete(&k, "ns", "name").unwrap();
    let replacement = r.create(&k, "ns", "name").unwrap();
    assert_ne!(replacement.1, original.1);
    drop(r);
    let current = fs::read(&path).unwrap();
    fs::write(&path, &old).unwrap();
    assert!(IncarnationStore::open(&path, anchor.clone()).is_err());
    let mut corrupt = current.clone();
    corrupt[40] ^= 1;
    fs::write(&path, corrupt).unwrap();
    assert!(IncarnationStore::open(&path, anchor.clone()).is_err());
    fs::write(&path, &current).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0; 37])
        .unwrap();
    let reopened = IncarnationStore::open(&path, anchor).unwrap();
    assert_eq!(reopened.recovered_tail_bytes, 37);
    drop(reopened);
    assert_eq!(fs::read(&path).unwrap(), current);
    fs::remove_file(path).unwrap();
}
#[test]
fn i1_disk_images_bind_full_incarnation_epoch_and_tombstone() {
    use envelope::*;
    let body = vec![7; 4096 - IMG_BYTES];
    let mut image = ExtentImage {
        tenant: 3,
        incarnation: 1u128 << 100,
        tombstone: false,
        layout_version: 1,
        checkpoint_lsn: 9,
        epoch: 2,
        image_seq: 4,
    };
    let encode = |i: ExtentImage| {
        [
            i.header_words(&body)
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>(),
            body.clone(),
        ]
        .concat()
    };
    let bytes = encode(image);
    assert!(ExtentImage::validate(&bytes, 3, image.incarnation, 2).is_some());
    assert!(ExtentImage::validate(&bytes, 3, 1, 2).is_none());
    assert!(ExtentImage::validate(&bytes, 3, image.incarnation, 1).is_none());
    image.tombstone = true;
    assert!(ExtentImage::validate(&encode(image), 3, image.incarnation, 2).is_none());
}
#[test]
fn q1_vectors_accept_4096_and_reject_4097() {
    for n in [256, 4096, 4097] {
        let v = vec!["0.1"; n].join(",");
        let q = format!("@1 similarity(vector: [{v}], metric: cosine, threshold: 0.8)");
        assert_eq!(parse_ciaql(&q).is_ok(), n <= 4096, "dims={n}");
    }
}
#[test]
fn q2_q3_q5_calls_refs_and_case_are_checked() {
    assert!(parse_ciaql("@1 mutate {state.x=evil(1)}").is_err());
    assert!(parse_ciaql("@1 mutate {state.x=$ref:0x12}").is_err());
    assert!(parse_ciaql("@1 mutate {state.x=$ref:0x12.abababababababababababababababab}").is_ok());
    for op in ["IN", "in", "In"] {
        assert!(parse_ciaql(&format!("@1 where state.x {op} [1,2]")).is_ok());
    }
    for op in ["CONTAINS", "contains", "Contains"] {
        assert!(parse_ciaql(&format!(r#"@1 where state.x {op} "x""#)).is_ok());
    }
}
#[test]
fn q4_public_deep_ast_validation_and_destruction_are_bounded() {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(|| {
            use ciagomem_ql::*;
            let mut tree = BoolExpr::And(vec![]);
            for _ in 0..50_000 {
                tree = BoolExpr::And(vec![tree]);
            }
            let q = Query {
                source: Source::Actor(1),
                stages: vec![Stage::Filter(tree)],
            };
            assert!(validate_query_ast(&q).is_err());
            drop(q);
        })
        .unwrap()
        .join()
        .unwrap();
}
#[test]
fn p1_p2_q6_destination_roots_and_tenant_scoped_validation() {
    assert!(destination_allowed(
        "s3://bucket/path/file",
        &["s3://bucket/"]
    ));
    assert!(destination_allowed(
        "/var/backups/ciagomem/a",
        &["/var/backups/ciagomem/"]
    ));
    let token = session(11);
    let owner = |_| Some(11);
    let auth = AuthContext {
        session: &token,
        owner_of: &owner,
        allowed_destinations: &[],
        owner_version: 8,
    };
    let q = parse_ciaql("scan actors[*]").unwrap();
    let v = validate_query(&q, &auth).unwrap();
    assert_eq!(v.scope(), QueryScope::Tenant(11));
    assert_eq!(v.owner_version(), 8);
}
#[test]
fn o1_duplicate_cumulative_credit_does_not_inflate_capacity() {
    let mut f = OutboundFlow::new(2, 2);
    let mut lane = (VecDeque::new(), 2);
    f.send(&mut lane, 1).unwrap();
    f.send(&mut lane, 2).unwrap();
    lane.0.pop_front();
    assert_eq!(f.acknowledge(1), Ok(true));
    assert_eq!(f.acknowledge(1), Ok(false));
    assert_eq!(f.acknowledge(0), Ok(false));
    assert!(f.acknowledge(3).is_err());
    f.send(&mut lane, 3).unwrap();
    assert_eq!(f.send(&mut lane, 4), Ok(SendOutcome::Deferred));
    let mesh = mesh::PriorityMesh::<u64, u64, 2, 2>::new(1);
    assert_eq!(mesh.control.producers.len(), 1);
}
#[test]
fn a1_a3_a4_s1_canonical_slot_checks_incarnation_and_equal_lsn() {
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
            123,
            0,
            0,
        )
        .unwrap()
        .unwrap();
    let _: slot::SlotHandle = h;
    assert!(
        a.read(h, 1, 124, Visibility::Applied, |_| Some(()))
            .is_err()
    );
    a.write(h, 5, |w| w.set(20, 8)).unwrap();
    assert_eq!(
        a.write(h, 5, |w| w.set(20, 9)),
        Err(WriteError::DuplicateLsn)
    );
    assert_eq!(std::mem::size_of::<SlotHeader>(), 128);
    a.core(0).fence();
    assert!(
        a.read(h, 1, 123, Visibility::Applied, |_| Some(()))
            .is_err()
    );
}
#[cfg(target_os = "linux")]
#[test]
fn p4_filesystem_adapter_blocks_symlinks_traversal_and_foreign_tenant() {
    use std::os::unix::fs::symlink;
    let path = temp("exports");
    let _ = fs::remove_dir_all(&path);
    fs::create_dir(&path).unwrap();
    symlink("/tmp", path.join("escape")).unwrap();
    let root = destination::LocalExportRoot::open(&path, 11, 1).unwrap();
    let token = session(11);
    assert!(root.create(&token, "../outside", 1).is_err());
    assert!(root.create(&token, "escape/outside", 1).is_err());
    assert!(root.create(&session(22), "foreign", 1).is_err());
    let file = root.create(&token, "safe", 1).unwrap();
    drop(file);
    assert!(root.create(&token, "safe", 1).is_err());
    fs::remove_dir_all(path).unwrap();
}
#[cfg(target_os = "linux")]
#[test]
fn x1_probe_rejects_unsealed_write_capable_memfd_and_budget_is_released() {
    use export_window::*;
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    let fd = unsafe {
        OwnedFd::from_raw_fd(libc::memfd_create(
            c"r5-negative".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        ))
    };
    assert_eq!(unsafe { libc::ftruncate(fd.as_raw_fd(), 4096) }, 0);
    assert!(verify_write_seal(fd.as_fd(), 4096).is_err());
    let budget = ExportBudget::new(4096, 1);
    let w = SealedExportWindow::create_and_selftest(4096, &budget).unwrap();
    assert_eq!(budget.usage(), (4096, 1));
    drop(w);
    assert_eq!(budget.usage(), (0, 0));
}

#[test]
fn f1_f2_f3_f4_f5_frame_permits_bind_reader_field_action_and_issuer() {
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let token = session(11);
    let label = support::label(Integrity::AgentDerived, 2, vec![11]).unwrap();
    let e = signer.endorse(Approval {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        nonce: 9,
        field: FrameField::ActiveIntent as u8,
        sources: 2,
        digest: content_digest(b"value"),
        sink: 11,
        tool: "send".into(),
        expires_at: 50,
    });
    let ctx = AdmissionContext {
        session: &token,
        reader: 11,
        content: b"value",
        now: 1,
        nonce: 9,
        tool: "send",
        sink: 11,
        verifier: &verifier,
    };
    assert!(
        admit_to_frame(
            FrameField::Playbook,
            label.clone(),
            Some(&e),
            &ctx,
            &mut NonceTracker::new(4, 128, 1)
        )
        .is_err()
    );
    let wrong = AdmissionContext { reader: 22, ..ctx };
    assert!(matches!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&e),
            &wrong,
            &mut NonceTracker::new(4, 128, 1)
        ),
        Err(FlowDenied::ReaderDenied)
    ));
    let wrong = AdmissionContext {
        tool: "delete",
        ..ctx
    };
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&e),
            &wrong,
            &mut NonceTracker::new(4, 128, 1)
        )
        .is_err()
    );
    let untrusted_key = AttestationSigner::from_seed([7; 32]).verifier();
    let wrong = AdmissionContext {
        verifier: &untrusted_key,
        ..ctx
    };
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&e),
            &wrong,
            &mut NonceTracker::new(4, 128, 1)
        )
        .is_err()
    );
    assert!(!gateway_allow(true, &label, 11));
    let mut permit = admit_to_frame(
        FrameField::ActiveIntent,
        label,
        Some(&e),
        &ctx,
        &mut NonceTracker::new(4, 128, 1),
    )
    .unwrap();
    assert!(authorize_tool_call(&mut permit, &token, "delete", 11, b"value", 2).is_err());
    assert!(authorize_tool_call(&mut permit, &token, "send", 11, b"other", 2).is_err());
    assert!(authorize_tool_call(&mut permit, &token, "send", 11, b"value", 2).is_ok());
}
#[test]
fn c2_session_and_audience_are_mandatory_and_nontransferable() {
    let k = KeyedPrf([9; 32]);
    let ctx = RequestCtx {
        tenant_incarnation: 11,
        key_epoch: 2,
        now_ns: 1,
        op_bit: OP_READ,
        peer_uid: 1000,
        session: 1,
        audience: 7,
        boot_epoch: 1,
    };
    let caveats = vec![
        Caveat::Tenant(11),
        Caveat::ExpiresAt(100),
        Caveat::PeerUid(1000),
        Caveat::Ops(OP_READ),
        Caveat::KeyEpoch(2),
    ];
    assert!(
        Macaroon::issue(&k, caveats.clone())
            .unwrap()
            .verify(&k, &ctx)
            .is_err()
    );
    let mut caveats = caveats;
    caveats.extend([
        Caveat::Session(1),
        Caveat::Audience(7),
        Caveat::BootEpoch(1),
    ]);
    let m = Macaroon::issue(&k, caveats).unwrap();
    assert!(m.verify(&k, &ctx).is_ok());
    assert!(m.verify(&k, &RequestCtx { session: 2, ..ctx }).is_err());
    assert!(
        m.verify(
            &k,
            &RequestCtx {
                audience: 8,
                boot_epoch: 1,
                ..ctx
            }
        )
        .is_err()
    );
}
