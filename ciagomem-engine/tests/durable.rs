#![cfg(unix)]
use ciagomem_core::*;
use ciagomem_engine::{host::*, links::OnDelete, persistence::DurableJournal, *};
use ciagomem_wal::gclog::{LogConfig, TailMode};
use std::{
    os::unix::fs::FileExt,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};
struct Time(Arc<AtomicU64>);
impl Clock for Time {
    fn now_ns(&self) -> u64 {
        self.0.load(Relaxed)
    }
}
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ciagomem-durable-{}", random_id().unwrap()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cfg() -> LogConfig {
    LogConfig {
        block: 4096,
        buf_bytes: 16384,
        buffers: 2,
        max_inflight: 1,
        min_batch_bytes: 4096,
        max_delay_ns: 0,
        capacity: 262144,
        file: 0,
        tail: TailMode::Pad,
        max_retries: 0,
    }
}
fn open(dir: &Dir) -> Result<Engine<DurableJournal>> {
    Engine::open_durable(
        &dir.0,
        cfg(),
        1 << 20,
        2,
        Limits::default(),
        schema::Schemas::new(),
        KeyedPrf([9; 32]),
        1,
        Arc::new(Time(Arc::new(AtomicU64::new(1)))),
    )
}
fn actor(id: u64) -> ActorRef {
    ActorRef {
        id,
        incarnation: 100 + id as u128,
    }
}
fn record(id: u64, n: i64) -> Record {
    Record::new(
        actor(id),
        11,
        [
            ("credits".into(), Scalar::Int(n)),
            ("body".into(), Scalar::Text("durable keyword".into())),
        ]
        .into(),
        Label::external(3, vec![11]).unwrap(),
    )
}
fn mac(expires: u64) -> Macaroon {
    Macaroon::issue(
        &KeyedPrf([7; 32]),
        vec![
            Caveat::Tenant(11),
            Caveat::ExpiresAt(expires),
            Caveat::PeerUid(42),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(1),
            Caveat::Ops(OP_READ | OP_WRITE | OP_OPERATOR),
            Caveat::KeyEpoch(1),
        ],
    )
    .unwrap()
}
fn token() -> VerifiedMacaroon {
    mac(100)
        .verify(
            &KeyedPrf([7; 32]),
            &RequestCtx {
                tenant_incarnation: 11,
                session: 1,
                peer_uid: 42,
                audience: 7,
                key_epoch: 1,
                boot_epoch: 1,
                op_bit: OP_READ,
                now_ns: 1,
            },
        )
        .unwrap()
}
#[test]
fn restart_rebuilds_scalar_text_vector_and_mutated_records() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    let mut r = record(1, 100);
    r.embedding = vec![1., 0., 0.];
    e.put(r, &token()).unwrap();
    e.execute(
        &ciagomem_ql::parse("@1 mutate {state.credits -= 25}").unwrap(),
        &token(),
    )
    .unwrap();
    drop(e);
    let mut e = open(&d).unwrap();
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(75)
    );
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.embedding,
        vec![1., 0., 0.]
    );
    let q = ciagomem_ql::parse(
        "scan actors[*] where state.credits == 75 select {credits: state.credits}",
    )
    .unwrap();
    assert_eq!(e.execute(&q, &token()).unwrap().rows.len(), 1);
    let hybrid=ciagomem_ql::parse("scan actors[*] search(field: body, query: \"keyword\", top_k: 10) similarity(vector: [1.0,0.0,0.0], metric: cosine, threshold: 0.9) select {credits: state.credits}").unwrap();
    assert_eq!(e.execute(&hybrid, &token()).unwrap().rows.len(), 1);
    let indexed = e.execute(&q, &token()).unwrap();
    let scanned = e
        .execute_with(
            &q,
            &token(),
            Options {
                disable_indexes: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(indexed.rows.len(), scanned.rows.len());
}
#[test]
fn links_tombstones_and_delete_cursor_survive_restart_and_checkpoint() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    e.put(record(1, 1), &token()).unwrap();
    e.put(record(2, 2), &token()).unwrap();
    e.add_link(actor(2), actor(1), OnDelete::Cascade, &token())
        .unwrap();
    let job = e.begin_delete(actor(1), &token()).unwrap();
    e.advance_delete(job.job, 0, 1, &token()).unwrap();
    e.checkpoint().unwrap();
    drop(e);
    let mut e = open(&d).unwrap();
    assert!(e.snapshot(actor(2), &token()).is_err());
    let status = e.delete_status(job.job, &token()).unwrap();
    assert_eq!(status.cursor, 1);
    assert_eq!(status.remaining, 1);
    e.advance_delete(job.job, 1, 1, &token()).unwrap();
    drop(e);
    let e = open(&d).unwrap();
    assert!(e.snapshot(actor(1), &token()).is_err());
    assert!(e.snapshot(actor(2), &token()).is_err());
}
#[test]
fn checkpoint_reclaims_ring_and_recovers_without_old_history() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    for n in 0..150 {
        e.put(record(1, n), &token()).unwrap();
        if n % 8 == 0 {
            e.checkpoint().unwrap();
        }
    }
    drop(e);
    let e = open(&d).unwrap();
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(149)
    );
}
#[test]
fn corrupted_acknowledged_wal_is_not_recovered_as_empty() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    e.put(record(1, 1), &token()).unwrap();
    drop(e);
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(d.0.join("engine.wal"))
        .unwrap();
    f.write_all_at(&[0; 32], 8192).unwrap();
    f.sync_all().unwrap();
    assert!(open(&d).is_err());
}
#[test]
fn wrong_key_and_damaged_checkpoints_fail_closed() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    e.put(record(1, 1), &token()).unwrap();
    e.checkpoint().unwrap();
    drop(e);
    assert!(
        Engine::open_durable(
            &d.0,
            cfg(),
            1 << 20,
            2,
            Limits::default(),
            schema::Schemas::new(),
            KeyedPrf([0; 32]),
            1,
            Arc::new(MonotonicClock)
        )
        .is_err()
    );
    for n in 0..2 {
        let p = d.0.join(format!("checkpoint.{n}"));
        if p.exists() {
            std::fs::write(p, b"corrupted").unwrap();
        }
    }
    assert!(open(&d).is_err());
}
#[test]
fn concurrent_owner_is_rejected_and_journal_rejection_keeps_index_unchanged() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    assert!(open(&d).is_err());
    e.put(record(1, 1), &token()).unwrap();
    let mut r = record(1, 2);
    r.fields
        .insert("body".into(), Scalar::Text("a".repeat(20000)));
    assert!(e.put(r, &token()).is_err());
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(1)
    );
    drop(e);
    let e = open(&d).unwrap();
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(1)
    );
}
#[test]
fn label_checkpoint_is_content_and_key_bound() {
    let key = KeyedPrf([1; 32]);
    let label = test_label(Integrity::Policy, 5, vec![11]).unwrap();
    let sealed = label.seal_checkpoint(&key, b"image");
    assert_eq!(
        Label::open_checkpoint(&key, b"image", &sealed).unwrap(),
        label
    );
    assert!(Label::open_checkpoint(&key, b"changed", &sealed).is_err());
    assert!(Label::open_checkpoint(&KeyedPrf([2; 32]), b"image", &sealed).is_err());
}
#[test]
fn host_reserves_capacity_until_reply_release_and_drains_shutdown() {
    let d = Dir::new();
    let clock = Arc::new(Time(Arc::new(AtomicU64::new(1))));
    let host = Host::spawn(
        open(&d).unwrap(),
        Authentication {
            key: KeyedPrf([7; 32]),
            audience: 7,
            key_epoch: 1,
            boot_epoch: 1,
            clock,
        },
        HostLimits {
            requests: 1,
            per_tenant_requests: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let c = host.client();
    let peer = || Peer {
        tenant: 11,
        session: 1,
        uid: 42,
    };
    let reply = c
        .submit(Operation::Put(record(1, 9)), mac(100), peer())
        .unwrap()
        .receive()
        .unwrap();
    assert!(reply.result.is_ok());
    assert!(matches!(
        c.submit(Operation::Delete(actor(1)), mac(100), peer()),
        Err(AdmissionError::Busy)
    ));
    drop(reply);
    assert!(
        c.submit(
            Operation::Query("@1 select {credits: state.credits}".into()),
            mac(100),
            peer()
        )
        .unwrap()
        .receive()
        .unwrap()
        .result
        .is_ok()
    );
    host.shutdown().unwrap();
    assert!(matches!(
        c.submit(Operation::Delete(actor(1)), mac(100), peer()),
        Err(AdmissionError::Closed)
    ));
    assert!(open(&d).unwrap().snapshot(actor(1), &token()).is_ok());
}
#[test]
fn host_rejects_expired_or_wrong_peer_credentials() {
    let d = Dir::new();
    let host = Host::spawn(
        open(&d).unwrap(),
        Authentication {
            key: KeyedPrf([7; 32]),
            audience: 7,
            key_epoch: 1,
            boot_epoch: 1,
            clock: Arc::new(Time(Arc::new(AtomicU64::new(10)))),
        },
        HostLimits::default(),
    )
    .unwrap();
    let c = host.client();
    assert!(matches!(
        c.submit(
            Operation::Put(record(1, 1)),
            mac(5),
            Peer {
                tenant: 11,
                session: 1,
                uid: 42
            }
        ),
        Err(AdmissionError::Permission)
    ));
    assert!(matches!(
        c.submit(
            Operation::Put(record(1, 1)),
            mac(100),
            Peer {
                tenant: 11,
                session: 1,
                uid: 43
            }
        ),
        Err(AdmissionError::Permission)
    ));
    host.shutdown().unwrap();
    assert!(open(&d).unwrap().snapshot(actor(1), &token()).is_err());
}

fn test_label(
    integrity: Integrity,
    sources: u64,
    readers: Vec<u128>,
) -> std::result::Result<Label, &'static str> {
    if integrity == Integrity::External {
        return Label::external(sources, readers);
    }
    let issuer = ciagomem_core::AttestationSigner::from_seed([3; 32]);
    let tenant = readers.first().copied().unwrap_or(11);
    let proof = issuer.provenance(ciagomem_core::Provenance {
        tenant,
        session: 1,
        boot_epoch: 1,
        integrity: integrity as u8,
        sources,
        readers,
        digest: ciagomem_core::content_digest(b"value"),
        expires_at: 1000,
    });
    Label::attested(&proof, &issuer.verifier(), tenant, 1, 1, b"value", 1)
}

#[test]
fn zeroed_existing_store_does_not_become_new() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    e.put(record(1, 1), &token()).unwrap();
    drop(e);
    std::fs::OpenOptions::new()
        .write(true)
        .open(d.0.join("engine.wal"))
        .unwrap()
        .set_len(0)
        .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(d.0.join("engine.ack"))
        .unwrap()
        .set_len(0)
        .unwrap();
    assert!(open(&d).is_err());
}
#[test]
fn interrupted_checkpoint_staging_is_cleaned_only_after_successful_recovery() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    e.put(record(1, 1), &token()).unwrap();
    drop(e);
    std::fs::write(d.0.join("checkpoint.pending"), b"partial").unwrap();
    let mut e = open(&d).unwrap();
    e.checkpoint().unwrap();
    assert!(!d.0.join("checkpoint.pending").exists());
}
#[test]
fn attacker_chosen_storage_mac_does_not_replace_provenance_signature() {
    let key = KeyedPrf([1; 32]);
    let label = test_label(Integrity::UserDirect, 5, vec![11]).unwrap();
    let mut sealed = label.seal_checkpoint(&key, b"image");
    sealed[4] = Integrity::Policy as u8;
    let split = sealed.len() - 32;
    let digest = content_digest(b"image");
    let tag = key.mac256(
        b"ciagomem/label-checkpoint/v1",
        &[&sealed[..split], &digest].concat(),
    );
    sealed[split..].copy_from_slice(&tag);
    assert!(Label::open_checkpoint(&key, b"image", &sealed).is_err());
}
#[test]
fn signed_labels_roundtrip_without_renewing_scope() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    let mut r = record(1, 1);
    r.label = test_label(Integrity::UserDirect, 5, vec![11]).unwrap();
    let label = r.label.clone();
    e.put(r, &token()).unwrap();
    e.checkpoint().unwrap();
    drop(e);
    assert_eq!(
        open(&d)
            .unwrap()
            .snapshot(actor(1), &token())
            .unwrap()
            .record
            .label,
        label
    );
}
#[test]
fn invalid_log_configuration_returns_error_without_panicking() {
    let d = Dir::new();
    let mut c = cfg();
    c.block = 0;
    assert!(
        Engine::open_durable(
            &d.0,
            c,
            1 << 20,
            2,
            Limits::default(),
            schema::Schemas::new(),
            KeyedPrf([9; 32]),
            1,
            Arc::new(MonotonicClock)
        )
        .is_err()
    );
}

#[test]
fn host_does_not_announce_readiness_when_recovery_fails() {
    let result = Host::spawn_with(
        || Err(Error::InvalidImage),
        Authentication {
            key: KeyedPrf([7; 32]),
            audience: 7,
            key_epoch: 1,
            boot_epoch: 1,
            clock: Arc::new(MonotonicClock),
        },
        HostLimits::default(),
    );
    assert!(result.is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn native_wal_integration_or_explicit_unavailable() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    match e.enable_uring() {
        Ok(()) => {}
        Err(error) if matches!(error.raw_os_error(), Some(1 | 13 | 38)) => {
            eprintln!("SKIPPED native WAL integration: io_uring unavailable: {error}");
            return;
        }
        Err(error) => panic!("unexpected native WAL initialization failure: {error}"),
    }
    e.put(record(1, 3), &token()).unwrap();
    drop(e);
    assert_eq!(
        open(&d)
            .unwrap()
            .snapshot(actor(1), &token())
            .unwrap()
            .record
            .fields["credits"],
        Scalar::Int(3)
    );
}

#[test]
fn full_wal_rejects_without_publishing_then_checkpoint_restores_admission() {
    let d = Dir::new();
    let mut e = open(&d).unwrap();
    let mut last = None;
    for n in 0..1000 {
        match e.put(record(1, n), &token()) {
            Ok(()) => last = Some(n),
            Err(error) => {
                assert_eq!(error, Error::Budget);
                break;
            }
        }
    }
    let last = last.unwrap();
    assert!(last < 999);
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(last)
    );
    e.checkpoint().unwrap();
    e.put(record(1, 9999), &token()).unwrap();
    drop(e);
    assert_eq!(
        open(&d)
            .unwrap()
            .snapshot(actor(1), &token())
            .unwrap()
            .record
            .fields["credits"],
        Scalar::Int(9999)
    );
}

#[test]
fn queued_request_rechecks_expiry_before_mutation() {
    use std::sync::{Mutex, mpsc};
    struct Gate {
        now: AtomicU64,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Clock for Gate {
        fn now_ns(&self) -> u64 {
            if std::thread::current().name() == Some("ciagomem-storage") {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
            }
            self.now.load(Relaxed)
        }
    }
    let d = Dir::new();
    let (entered, wait) = mpsc::sync_channel(1);
    let (release, receiver) = mpsc::sync_channel(1);
    let gate = Arc::new(Gate {
        now: AtomicU64::new(1),
        entered,
        release: Mutex::new(receiver),
    });
    let host = Host::spawn(
        open(&d).unwrap(),
        Authentication {
            key: KeyedPrf([7; 32]),
            audience: 7,
            key_epoch: 1,
            boot_epoch: 1,
            clock: gate.clone(),
        },
        HostLimits::default(),
    )
    .unwrap();
    let ticket = host
        .client()
        .submit(
            Operation::Put(record(1, 1)),
            mac(100),
            Peer {
                tenant: 11,
                session: 1,
                uid: 42,
            },
        )
        .unwrap();
    wait.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    gate.now.store(100, Relaxed);
    release.send(()).unwrap();
    assert!(matches!(
        ticket.receive().unwrap().result,
        Err(Error::Permission)
    ));
    host.shutdown().unwrap();
    assert!(open(&d).unwrap().snapshot(actor(1), &token()).is_err());
}

#[test]
fn recovery_budget_backpressures_before_acknowledging_an_unreopenable_history() {
    let d = Dir::new();
    let bounded = || {
        Engine::open_durable(
            &d.0,
            cfg(),
            4096,
            2,
            Limits::default(),
            schema::Schemas::new(),
            KeyedPrf([9; 32]),
            1,
            Arc::new(Time(Arc::new(AtomicU64::new(1)))),
        )
    };
    let mut e = bounded().unwrap();
    let mut last = 0;
    for n in 0..100 {
        match e.put(record(1, n), &token()) {
            Ok(()) => last = n,
            Err(error) => {
                assert_eq!(error, Error::Budget);
                break;
            }
        }
    }
    assert!(last < 99);
    drop(e);
    let mut e = bounded().unwrap();
    assert_eq!(
        e.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(last)
    );
    e.checkpoint().unwrap();
    e.put(record(1, 9999), &token()).unwrap();
    drop(e);
    assert!(bounded().is_ok());
}

#[test]
fn process_child_writer() {
    let Some(directory) = std::env::var_os("CIAGOMEM_DURABLE_CHILD_DIR") else {
        return;
    };
    let d = Dir(PathBuf::from(directory));
    let mut engine = open(&d).unwrap();
    engine.put(record(1, 47), &token()).unwrap();
    // Deliberately bypass Rust destructors: only completed persistence may survive.
    std::process::exit(0);
}
#[test]
fn separate_process_recovers_acknowledged_write_without_destructors() {
    let d = Dir::new();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_child_writer", "--nocapture"])
        .env("CIAGOMEM_DURABLE_CHILD_DIR", &d.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let engine = open(&d).unwrap();
    assert_eq!(
        engine.snapshot(actor(1), &token()).unwrap().record.fields["credits"],
        Scalar::Int(47)
    );
}
