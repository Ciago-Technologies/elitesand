#![cfg(unix)]
use ciagomem_core::*;
use ciagomem_engine::{persistence::DurableJournal, retry::*, *};
use ciagomem_wal::gclog::{LogConfig, TailMode};
use std::{path::PathBuf, sync::Arc};
struct Clock1;
impl Clock for Clock1 {
    fn now_ns(&self) -> u64 {
        1
    }
}
struct Dir(PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("ciagomem-retries-{}", random_id().unwrap()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn open(d: &Dir, boot: u128) -> Engine<DurableJournal> {
    Engine::open_durable(
        &d.0,
        LogConfig {
            block: 4096,
            buf_bytes: 32768,
            buffers: 2,
            max_inflight: 1,
            min_batch_bytes: 4096,
            max_delay_ns: 0,
            capacity: 262144,
            file: 0,
            tail: TailMode::Pad,
            max_retries: 0,
        },
        2 << 20,
        1,
        Limits::default(),
        schema::Schemas::new(),
        KeyedPrf([9; 32]),
        boot,
        Arc::new(Clock1),
    )
    .unwrap()
}
fn token(boot: u128, client: u64) -> VerifiedMacaroon {
    let m = Macaroon::issue(
        &KeyedPrf([7; 32]),
        vec![
            Caveat::Tenant(11),
            Caveat::Session(1),
            Caveat::PeerUid(42),
            Caveat::Audience(7),
            Caveat::KeyEpoch(1),
            Caveat::BootEpoch(boot),
            Caveat::ExpiresAt(1000),
            Caveat::Ops(OP_READ | OP_WRITE),
            Caveat::ClientId(client),
        ],
    )
    .unwrap();
    let bytes = m.to_bytes().unwrap();
    Macaroon::from_bytes(&bytes)
        .unwrap()
        .verify(
            &KeyedPrf([7; 32]),
            &RequestCtx {
                tenant_incarnation: 11,
                session: 1,
                peer_uid: 42,
                audience: 7,
                key_epoch: 1,
                boot_epoch: boot,
                op_bit: OP_READ,
                now_ns: 1,
            },
        )
        .unwrap()
}
fn key(sequence: u64) -> RequestKey {
    RequestKey {
        tenant: 11,
        client: 99,
        sequence,
    }
}
fn record() -> Record {
    Record::new(
        ActorRef {
            id: 1,
            incarnation: 1,
        },
        11,
        [("credits".into(), Scalar::Int(100))].into(),
        Label::external(1, vec![11]).unwrap(),
    )
}
fn create(e: &mut Engine<DurableJournal>) -> (ActorRef, u64) {
    let receipt = e
        .execute_retry(key(1), WriteCommand::Create(record()), &token(1, 99))
        .unwrap();
    assert_eq!(receipt.status, 0);
    let actor = ActorRef {
        id: u64::from_le_bytes(receipt.result[..8].try_into().unwrap()),
        incarnation: u128::from_le_bytes(receipt.result[8..24].try_into().unwrap()),
    };
    (actor, receipt.commit_sequence)
}
#[test]
fn repeated_mutation_after_checkpoint_and_restart_is_not_reexecuted() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let (actor, version) = create(&mut e);
    let command = WriteCommand::Mutate {
        actor,
        expected_version: version,
        query: format!("@{} mutate {{state.credits -= 25}}", actor.id),
    };
    let first = e
        .execute_retry(key(2), command.clone(), &token(1, 99))
        .unwrap();
    assert_eq!(first.status, 0);
    assert_eq!(
        e.snapshot(actor, &token(1, 99)).unwrap().record.fields["credits"],
        Scalar::Int(75)
    );
    e.checkpoint().unwrap();
    drop(e);
    let mut e = open(&d, 2);
    let retry = e.execute_retry(key(2), command, &token(2, 99)).unwrap();
    assert_eq!(retry, first);
    assert_eq!(
        e.snapshot(actor, &token(2, 99)).unwrap().record.fields["credits"],
        Scalar::Int(75)
    );
    assert_eq!(
        e.document_version(actor, &token(2, 99)).unwrap(),
        first.commit_sequence
    );
}
#[test]
fn conflicting_key_and_sequence_gap_fail_closed() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    create(&mut e);
    let mut different = record();
    different.fields.insert("credits".into(), Scalar::Int(1));
    assert_eq!(
        e.execute_retry(key(1), WriteCommand::Create(different), &token(1, 99)),
        Err(Error::IdempotencyConflict)
    );
    assert_eq!(
        e.execute_retry(key(3), WriteCommand::Create(record()), &token(1, 99)),
        Err(Error::SequenceGap)
    );
}
#[test]
fn token_client_binding_is_required_before_cached_lookup() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    create(&mut e);
    assert_eq!(
        e.execute_retry(key(1), WriteCommand::Create(record()), &token(1, 100)),
        Err(Error::Permission)
    );
}
#[test]
fn terminal_failure_remains_failure_after_state_changes() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let (actor, version) = create(&mut e);
    let command = WriteCommand::Mutate {
        actor,
        expected_version: version + 99,
        query: format!("@{} mutate {{state.credits -= 25}}", actor.id),
    };
    let failed = e
        .execute_retry(key(2), command.clone(), &token(1, 99))
        .unwrap();
    assert_eq!(failed.status, 5);
    e.put(record(), &token(1, 99)).unwrap();
    drop(e);
    let mut e = open(&d, 2);
    assert_eq!(
        e.execute_retry(key(2), command, &token(2, 99)).unwrap(),
        failed
    );
}
#[test]
fn delete_retry_does_not_delete_recreated_actor() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let (actor, version) = create(&mut e);
    let cmd = WriteCommand::Delete {
        actor,
        expected_version: version,
    };
    let result = e.execute_retry(key(2), cmd.clone(), &token(1, 99)).unwrap();
    assert_eq!(result.status, 0);
    let mut recreated = record();
    recreated.actor.id = actor.id;
    recreated.actor.incarnation = 999;
    e.put(recreated, &token(1, 99)).unwrap();
    let version = e
        .document_version(
            ActorRef {
                id: actor.id,
                incarnation: 999,
            },
            &token(1, 99),
        )
        .unwrap();
    assert_eq!(e.execute_retry(key(2), cmd, &token(1, 99)).unwrap(), result);
    assert_eq!(
        e.document_version(
            ActorRef {
                id: actor.id,
                incarnation: 999
            },
            &token(1, 99)
        )
        .unwrap(),
        version
    );
}
#[test]
fn false_predicate_has_durable_terminal_receipt() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let (actor, version) = create(&mut e);
    let cmd = WriteCommand::Mutate {
        actor,
        expected_version: version,
        query: format!(
            "@{} where state.credits < 0 mutate {{state.credits -= 25}}",
            actor.id
        ),
    };
    let receipt = e.execute_retry(key(2), cmd.clone(), &token(1, 99)).unwrap();
    assert_eq!(receipt.status, 6);
    drop(e);
    let mut e = open(&d, 2);
    assert_eq!(
        e.execute_retry(key(2), cmd, &token(2, 99)).unwrap(),
        receipt
    );
    assert_eq!(
        e.snapshot(actor, &token(2, 99)).unwrap().record.fields["credits"],
        Scalar::Int(100)
    );
}

#[test]
fn authorization_failure_never_consumes_a_client_sequence() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let mut foreign = record();
    foreign.tenant = 22;
    assert_eq!(
        e.execute_retry(key(1), WriteCommand::Create(foreign), &token(1, 99)),
        Err(Error::ForeignActor)
    );
    assert_eq!(
        e.execute_retry(key(1), WriteCommand::Create(record()), &token(1, 99))
            .unwrap()
            .status,
        0
    );
}
#[test]
fn network_mutation_rejects_post_mutation_filters_before_effects() {
    let d = Dir::new();
    let mut e = open(&d, 1);
    let (actor, version) = create(&mut e);
    let cmd = WriteCommand::Mutate {
        actor,
        expected_version: version,
        query: format!(
            "@{} mutate {{state.credits -= 25}} |> where state.credits < 0",
            actor.id
        ),
    };
    assert_eq!(
        e.execute_retry(key(2), cmd, &token(1, 99)).unwrap().status,
        14
    );
    assert_eq!(e.document_version(actor, &token(1, 99)).unwrap(), version);
    assert_eq!(
        e.snapshot(actor, &token(1, 99)).unwrap().record.fields["credits"],
        Scalar::Int(100)
    );
}
