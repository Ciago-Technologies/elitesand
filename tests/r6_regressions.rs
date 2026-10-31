//! Corrected regressions adapted from reviewer R6 probes.
//!
//! PASS now means the reviewed defect is prevented; originals remain under review/r6.
mod support;
use coagomem_core::*;
use std::{
    collections::HashMap,
    fs,
    sync::{Arc, Mutex},
};

fn session(tenant: u128) -> VerifiedMacaroon {
    let k = KeyedPrf([9; 32]);
    Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(tenant),
            Caveat::ExpiresAt(1000),
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

fn tw() -> TenantWord {
    TenantWord {
        tenant: 1,
        core: 0,
        layout_version: 1,
        flags: 0,
    }
}

#[test]
fn n1_durability_and_fence_span_all_size_classes_but_not_other_cores() {
    let controls = CoreControl::shared(2);
    let class0 = SlotArena::<512>::new(0, 2, controls.clone());
    let class1 = SlotArena::<512>::new(1, 2, controls.clone());
    let h = class1.install(0, tw(), 123, 5, 0).unwrap().unwrap();
    let other = class1
        .install(1, TenantWord { core: 1, ..tw() }, 124, 5, 0)
        .unwrap()
        .unwrap();
    let p0 = slot::DurabilityPublisher::claim(controls.clone(), 0).unwrap();
    let p1 = slot::DurabilityPublisher::claim(controls.clone(), 1).unwrap();
    p0.publish(5);
    p1.publish(5);
    assert_eq!(class1.core(0).durable(), 5);
    assert!(
        class1
            .read(h, 1, 123, Visibility::Durable, |_| Some(()))
            .is_ok()
    );
    // The fence is raised during the read; the post-validation check must reject it.
    assert_eq!(
        class1
            .read(h, 1, 123, Visibility::Applied, |_| {
                class0.core(0).fence();
                Some(())
            })
            .unwrap_err(),
        SlotReadError::Fenced
    );
    p0.publish(u64::MAX);
    assert_eq!(class1.core(0).durable(), 5);
    assert!(
        class1
            .read(other, 1, 124, Visibility::Durable, |_| Some(()))
            .is_ok()
    );
    assert!(std::ptr::eq(class0.core(0), class1.core(0)));
}

#[test]
fn n2_derived_content_requires_fresh_signed_provenance() {
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let token = session(11);
    let attested = support::label(Integrity::AgentDerived, 2, vec![11]).unwrap();
    let evil = b"attacker chosen bytes never signed by the issuer";
    let laundered = Label::derived(evil, &[attested]);
    let ctx = AdmissionContext {
        session: &token,
        reader: 11,
        content: evil,
        now: 1,
        nonce: 1,
        tool: "none",
        sink: 11,
        verifier: &verifier,
    };
    let admitted = admit_to_frame(
        FrameField::Fact,
        laundered,
        None,
        &ctx,
        &mut NonceTracker::new(4, 128, 1),
    );
    assert!(matches!(admitted, Err(FlowDenied::InvalidProvenance)));
    let proof = signer.provenance(Provenance {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        integrity: Integrity::AgentDerived as u8,
        sources: 2,
        readers: vec![11],
        digest: content_digest(evil),
        expires_at: 100,
    });
    let signed = Label::attested(&proof, &verifier, 11, 1, 1, evil, 1).unwrap();
    assert!(
        admit_to_frame(
            FrameField::Fact,
            signed,
            None,
            &ctx,
            &mut NonceTracker::new(4, 128, 1)
        )
        .is_ok()
    );
}

#[derive(Default)]
struct SnapAnchor(Mutex<HashMap<[u8; 16], IdentityHead>>);
impl IdentityAnchor for SnapAnchor {
    fn initialize(&self, id: [u8; 16]) -> std::io::Result<()> {
        self.0.lock().unwrap().insert(id, IdentityHead::default());
        Ok(())
    }
    fn head(&self, id: [u8; 16]) -> std::io::Result<IdentityHead> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .ok_or_else(|| std::io::Error::other("unknown"))
    }
    fn advance(&self, id: [u8; 16], e: IdentityHead, n: IdentityHead) -> std::io::Result<()> {
        let mut m = self.0.lock().unwrap();
        if m.get(&id) != Some(&e) {
            return Err(std::io::Error::other("conflict"));
        }
        m.insert(id, n);
        Ok(())
    }
}

#[test]
fn n3_joint_restore_does_not_repeat_new_incarnations() {
    let path = std::env::temp_dir().join(format!("ciagomem-r6-n3-{}", std::process::id()));
    let _ = fs::remove_file(&path);
    let anchor = Arc::new(SnapAnchor::default());
    let k = KeyedPrf([9; 32]);
    let mut r = TenantRegistry::new(IncarnationStore::create(&path, anchor.clone()).unwrap());
    r.create(&k, "ns", "t").unwrap();
    drop(r);
    // Coordinated snapshot of BOTH journal and anchor state.
    let journal_snapshot = fs::read(&path).unwrap();
    let anchor_snapshot = anchor.0.lock().unwrap().clone();
    let mut r = TenantRegistry::new(IncarnationStore::open(&path, anchor.clone()).unwrap());
    r.delete(&k, "ns", "t").unwrap();
    let second_life = r.create(&k, "ns", "t").unwrap();
    drop(r);
    // Restore both together.
    fs::write(&path, &journal_snapshot).unwrap();
    *anchor.0.lock().unwrap() = anchor_snapshot;
    let mut r = TenantRegistry::new(IncarnationStore::open(&path, anchor.clone()).unwrap());
    r.delete(&k, "ns", "t").unwrap();
    let replayed = r.create(&k, "ns", "t").unwrap();
    assert_eq!(replayed.0, second_life.0);
    assert_ne!(replayed.1, second_life.1);
    let _ = fs::remove_file(&path);
}

#[test]
fn n4_ttl_quota_expiry_and_boot_epoch_are_enforced() {
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let mut tracker = NonceTracker::new(1, 2, 1);
    let admit = |tenant, nonce, now, expires, tracker: &mut NonceTracker| {
        let token = session(tenant);
        let label = support::label(Integrity::AgentDerived, 2, vec![tenant]).unwrap();
        let e = signer.endorse(Approval {
            tenant,
            session: 1,
            boot_epoch: 1,
            nonce,
            field: FrameField::ActiveIntent as u8,
            sources: 2,
            digest: content_digest(b"value"),
            sink: tenant,
            tool: "send".into(),
            expires_at: expires,
        });
        let ctx = AdmissionContext {
            session: &token,
            reader: tenant,
            content: b"value",
            now,
            nonce,
            tool: "send",
            sink: tenant,
            verifier: &verifier,
        };
        admit_to_frame(FrameField::ActiveIntent, label, Some(&e), &ctx, tracker)
    };
    assert!(matches!(
        admit(11, 1, 1, u64::MAX, &mut tracker),
        Err(FlowDenied::InvalidEndorsement)
    ));
    assert!(admit(11, 1, 1, 100, &mut tracker).is_ok());
    assert!(matches!(
        admit(11, 1, 2, 100, &mut tracker),
        Err(FlowDenied::Replay)
    ));
    assert!(matches!(
        admit(11, 2, 2, 100, &mut tracker),
        Err(FlowDenied::Capacity)
    ));
    assert!(admit(22, 1, 2, 100, &mut tracker).is_ok());
    assert!(admit(11, 2, 100, 200, &mut tracker).is_ok());
    let mut restarted = NonceTracker::new(1, 2, 2);
    assert!(matches!(
        admit(11, 1, 2, 100, &mut restarted),
        Err(FlowDenied::Replay)
    ));
    let epoch = SessionEpoch::new().unwrap();
    let next = SessionEpoch::new().unwrap();
    assert_ne!(epoch.id(), next.id());
}

#[test]
fn n7_checkpoint_barrier_releases_hot_tenant_pin_and_preserves_failed_pins() {
    use dirty::DirtyError;
    let mut pins = TruncationPins::new(1);
    pins.on_dirty(0, 10, 0).unwrap();
    for lsn in 11..1000 {
        let image = pins.begin_checkpoint(0).unwrap();
        assert_eq!(
            pins.check_admission(0),
            Err(DirtyError::CheckpointInProgress)
        );
        pins.on_checkpointed(0, image).unwrap();
        assert_eq!(pins.min_needed_lsn(lsn), lsn);
        pins.on_dirty(0, lsn, 0).unwrap();
    }
    pins.begin_checkpoint(0).unwrap();
    pins.checkpoint_failed(0).unwrap();
    assert_eq!(pins.min_needed_lsn(5000), 999);
    pins.on_dirty(0, 1000, 0).unwrap();
    assert!(pins.on_checkpointed(0, 999).is_err());
    assert_eq!(pins.min_needed_lsn(5000), 999);
}

#[test]
fn guard_label_derived_cannot_escalate_above_parent() {
    let ext = Label::external(1, vec![11]).unwrap();
    let d = Label::derived(b"x", &[ext]);
    assert_eq!(d.integrity(), Integrity::External);
}

fn checkpoint_image(lsn: u64, epoch: u64) -> Vec<u8> {
    use envelope::*;
    let mut bytes = vec![0; 4096];
    bytes[160..168].copy_from_slice(&lsn.to_le_bytes());
    let image = ExtentImage {
        tenant: 1,
        incarnation: 123,
        tombstone: false,
        layout_version: 1,
        checkpoint_lsn: lsn,
        epoch,
        image_seq: lsn,
    };
    for (i, w) in image.header_words(&bytes[IMG_BYTES..]).iter().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
    }
    bytes
}
#[test]
fn n5_n6_recovery_distinguishes_absence_corruption_and_covered_single_copy() {
    use io::{Outcome, SimIo};
    use lifecycle::*;
    use slot::RouteTable;
    // absent, both corrupt, one durable copy, torn next copy, missing WAL, two valid copies,
    // image ahead of recovered WAL, future image epoch and trusted-epoch mismatch.
    for case in 0..12 {
        let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
        let route = RouteTable::new(4);
        let mut lc = SlotLifecycle::new(
            &arena,
            &route,
            0,
            0..2,
            LifecycleConfig {
                low_free: 0,
                high_free: 1,
                max_flush_inflight: 1,
                max_retries: 2,
                epoch: 2,
            },
        );
        let mut disk = SimIo::new(1, 8 * 4096);
        let loc = ExtentLoc::new(0, 2, false);
        let mut bounds = RecoveryBounds {
            current_epoch: 2,
            expected_image_epoch: None,
            wal_start_lsn: 6,
            wal_end_lsn: 8,
        };
        if case != 0 && case != 10 {
            disk.files[0][8192..12288].copy_from_slice(&checkpoint_image(5, 1));
        }
        match case {
            1 => {
                disk.files[0][8192] ^= 1;
                disk.files[0][12288] = 0xff;
            }
            3 => disk.files[0][12288] = 0xff,
            4 => bounds.wal_start_lsn = 7,
            5 => disk.files[0][12288..16384].copy_from_slice(&checkpoint_image(7, 1)),
            6 => bounds.wal_end_lsn = 4,
            7 => bounds.current_epoch = 0,
            8 => bounds.expected_image_epoch = Some(2),
            9 => bounds.wal_end_lsn = 5,
            10 => bounds.expected_image_epoch = Some(1),
            _ => {}
        }
        let admission = lc.admit_load(1, 123, bounds, loc, 1, true, &mut disk);
        if case == 6 || case == 7 {
            assert_eq!(admission, Err(AdmitError::InvalidRecovery));
            continue;
        }
        admission.unwrap();
        let mut events = Vec::new();
        while !disk.inflight.is_empty() {
            let (ud, res) = disk.complete(0, Outcome::Ok);
            /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
            unsafe { lc.on_cqe(ud, res, &mut disk, &mut events) };
        }
        if case == 0 {
            assert!(events.contains(&Event::Absent { tenant: 1 }));
            continue;
        }
        if [1, 4, 8, 10].contains(&case) {
            assert!(
                events.contains(&Event::NeedsRepair { tenant: 1 }),
                "case={case}: {events:?}"
            );
            assert!(!events.iter().any(|e| matches!(
                e,
                Event::Absent { .. } | Event::Loaded { .. } | Event::ReplayRequired { .. }
            )));
            continue;
        }
        if case == 9 {
            assert!(events.iter().any(|e| matches!(
                e,
                Event::Loaded {
                    checkpoint_lsn: 5,
                    ..
                }
            )));
            continue;
        }
        assert!(route.lookup(1).unwrap().0.is_null());
        let (handle, from, through) = events
            .iter()
            .find_map(|e| match e {
                Event::ReplayRequired {
                    handle,
                    from_lsn,
                    through_lsn,
                    ..
                } => Some((*handle, *from_lsn, *through_lsn)),
                _ => None,
            })
            .expect("covered checkpoint requires replay");
        assert_eq!(from, if case == 5 { 8 } else { 6 });
        assert_eq!(through, 8);
        if case == 11 {
            assert!(lc.abort_replay(handle, &mut events));
            assert!(route.lookup(1).unwrap().0.is_null());
            assert_eq!(lc.free_slots(), 2);
            assert!(!lc.finish_replay(handle, &mut events));
            continue;
        }

        assert!(!lc.finish_replay(handle, &mut events));
        assert_eq!(lc.mutate(handle, 8, |_| ()), Err(Gate::Stale));
        for lsn in from..=through {
            lc.replay(handle, lsn, |w| w.set(20, lsn)).unwrap();
        }
        assert!(lc.finish_replay(handle, &mut events));
        assert_eq!(route.lookup(1).unwrap().0, handle);
        assert_eq!(
            arena
                .read(handle, 1, 123, Visibility::Applied, |w| w.word(20))
                .unwrap()
                .value,
            8
        );
    }
}
#[test]
fn n4_old_approval_is_denied_even_with_a_new_boot_token_and_reused_session_id() {
    let k = KeyedPrf([9; 32]);
    let token = Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(11),
            Caveat::ExpiresAt(1000),
            Caveat::PeerUid(1000),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(2),
            Caveat::Ops(OP_READ),
            Caveat::KeyEpoch(2),
        ],
    )
    .unwrap()
    .verify(
        &k,
        &RequestCtx {
            tenant_incarnation: 11,
            key_epoch: 2,
            now_ns: 1,
            op_bit: OP_READ,
            peer_uid: 1000,
            session: 1,
            audience: 7,
            boot_epoch: 2,
        },
    )
    .unwrap();
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let proof = signer.provenance(Provenance {
        tenant: 11,
        session: 1,
        boot_epoch: 2,
        integrity: Integrity::AgentDerived as u8,
        sources: 2,
        readers: vec![11],
        digest: content_digest(b"value"),
        expires_at: 100,
    });
    let label = Label::attested(&proof, &verifier, 11, 1, 2, b"value", 1).unwrap();
    let old = signer.endorse(Approval {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        nonce: 1,
        field: FrameField::ActiveIntent as u8,
        sources: 2,
        digest: content_digest(b"value"),
        sink: 11,
        tool: "send".into(),
        expires_at: 100,
    });
    let ctx = AdmissionContext {
        session: &token,
        reader: 11,
        content: b"value",
        now: 1,
        nonce: 1,
        tool: "send",
        sink: 11,
        verifier: &verifier,
    };
    assert!(matches!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label.clone(),
            Some(&old),
            &ctx,
            &mut NonceTracker::new(4, 2, 2)
        ),
        Err(FlowDenied::InvalidEndorsement)
    ));
    let mut current = old.approval.clone();
    current.boot_epoch = 2;
    assert!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label,
            Some(&signer.endorse(current)),
            &ctx,
            &mut NonceTracker::new(4, 2, 2)
        )
        .is_ok()
    );
}

#[test]
fn n7_lifecycle_and_pin_barriers_defer_writes_until_checkpoint_completion() {
    use io::{Outcome, SimIo};
    use lifecycle::*;
    use slot::RouteTable;
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(
        &arena,
        &route,
        0,
        0..2,
        LifecycleConfig {
            low_free: 0,
            high_free: 1,
            max_flush_inflight: 1,
            max_retries: 2,
            epoch: 1,
        },
    );
    let mut pins = TruncationPins::new(4);
    let mut disk = SimIo::new(1, 8 * 4096);
    let mut events = Vec::new();
    let handle = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut events)
        .unwrap();
    pins.on_dirty(1, 5, 0).unwrap();
    for lsn in 6..20 {
        let checkpoint = pins.begin_checkpoint(1).unwrap();
        assert!(lc.checkpoint(handle, checkpoint, &mut disk));
        assert_eq!(lc.mutate(handle, lsn, |w| w.set(20, lsn)), Err(Gate::Defer));
        assert_eq!(
            pins.check_admission(1),
            Err(dirty::DirtyError::CheckpointInProgress)
        );
        let (ud, res) = disk.complete(0, Outcome::Ok);
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { lc.on_cqe(ud, res, &mut disk, &mut events) };
        pins.on_checkpointed(1, checkpoint).unwrap();
        assert_eq!(pins.min_needed_lsn(lsn), lsn);
        lc.mutate(handle, lsn, |w| w.set(20, lsn)).unwrap();
        pins.on_dirty(1, lsn, 0).unwrap();
    }
    let checkpoint = pins.begin_checkpoint(1).unwrap();
    assert!(lc.checkpoint(handle, checkpoint, &mut disk));
    let (ud, res) = disk.complete(0, Outcome::Err(libc::EIO));
    /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
    unsafe { lc.on_cqe(ud, res, &mut disk, &mut events) };
    pins.checkpoint_failed(1).unwrap();
    assert_eq!(pins.min_needed_lsn(100), 19);
}

#[test]
fn n4_action_permit_cannot_outlive_its_authenticated_session() {
    let token = session(11);
    let signer = AttestationSigner::from_seed([3; 32]);
    let verifier = signer.verifier();
    let proof = signer.provenance(Provenance {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        integrity: Integrity::AgentDerived as u8,
        sources: 2,
        readers: vec![11],
        digest: content_digest(b"value"),
        expires_at: 5000,
    });
    let label = Label::attested(&proof, &verifier, 11, 1, 1, b"value", 1).unwrap();
    let approval = signer.endorse(Approval {
        tenant: 11,
        session: 1,
        boot_epoch: 1,
        nonce: 1,
        field: FrameField::ActiveIntent as u8,
        sources: 2,
        digest: content_digest(b"value"),
        sink: 11,
        tool: "send".into(),
        expires_at: 2000,
    });
    let ctx = AdmissionContext {
        session: &token,
        reader: 11,
        content: b"value",
        now: 1,
        nonce: 1,
        tool: "send",
        sink: 11,
        verifier: &verifier,
    };
    let mut permit = admit_to_frame(
        FrameField::ActiveIntent,
        label.clone(),
        Some(&approval),
        &ctx,
        &mut NonceTracker::new(4, 2, 1),
    )
    .unwrap();
    assert!(authorize_tool_call(&mut permit, &token, "send", 11, b"value", 1000).is_err());
    let expired = AdmissionContext { now: 1000, ..ctx };
    assert!(matches!(
        admit_to_frame(
            FrameField::ActiveIntent,
            label,
            Some(&approval),
            &expired,
            &mut NonceTracker::new(4, 2, 1)
        ),
        Err(FlowDenied::ExpiredSession)
    ));
}
