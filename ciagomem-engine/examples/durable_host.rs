//! Provider-free local integration example; keys stay in memory across the demonstrated reopen.
use ciagomem_core::*;
use ciagomem_engine::{host::*, persistence::DurableJournal, *};
use ciagomem_wal::gclog::{LogConfig, TailMode};
use std::{os::unix::fs::DirBuilderExt, sync::Arc};
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    fn key() -> [u8; 32] {
        let mut bytes = [0; 32];
        bytes[..16].copy_from_slice(&random_id().unwrap().to_le_bytes());
        bytes[16..].copy_from_slice(&random_id().unwrap().to_le_bytes());
        bytes
    }
    let directory = std::env::temp_dir().join(format!("ciagomem-example-{}", random_id()?));
    std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let storage_key = key();
    let auth_key = key();
    let boot = SessionEpoch::new()?.id();
    let clock: Arc<dyn Clock + Send + Sync> = Arc::new(MonotonicClock);
    let config = LogConfig {
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
    };
    let open = |boot| {
        Engine::open_durable(
            &directory,
            config,
            1 << 20,
            1,
            Limits::default(),
            schema::Schemas::new(),
            KeyedPrf(storage_key),
            boot,
            clock.clone(),
        )
    };
    let engine = open(boot).map_err(|e| format!("recovery: {e:?}"))?;
    let host = Host::spawn(
        engine,
        Authentication {
            key: KeyedPrf(auth_key),
            audience: 7,
            key_epoch: 1,
            boot_epoch: boot,
            clock: clock.clone(),
        },
        HostLimits::default(),
    )?;
    let token = Macaroon::issue(
        &KeyedPrf(auth_key),
        vec![
            Caveat::Tenant(11),
            Caveat::Session(1),
            Caveat::PeerUid(42),
            Caveat::Audience(7),
            Caveat::KeyEpoch(1),
            Caveat::BootEpoch(boot),
            Caveat::ExpiresAt(u64::MAX),
            Caveat::Ops(OP_READ | OP_WRITE | OP_OPERATOR),
        ],
    )
    .map_err(|e| format!("issuer: {e:?}"))?;
    let client = host.client();
    let peer = || Peer {
        tenant: 11,
        session: 1,
        uid: 42,
    };
    let actor = ActorRef {
        id: 1,
        incarnation: random_id()?,
    };
    let record = Record::new(
        actor,
        11,
        [("credits".into(), Scalar::Int(100))].into(),
        Label::external(1, vec![11])?,
    );
    for operation in [
        Operation::Put(record),
        Operation::Query("@1 mutate {state.credits -= 25}".into()),
        Operation::Checkpoint,
    ] {
        let reply = client
            .submit(operation, token.clone(), peer())
            .map_err(|e| format!("admission: {e:?}"))?
            .receive()?;
        if let Err(error) = reply.result {
            return Err(format!("execution: {error:?}").into());
        }
    }
    host.shutdown().map_err(|_| "storage worker panicked")?;
    let new_boot = SessionEpoch::new()?.id();
    let mut recovered: Engine<DurableJournal> =
        open(new_boot).map_err(|e| format!("reopen: {e:?}"))?;
    let mut renewed = token;
    renewed = Macaroon::issue(
        &KeyedPrf(auth_key),
        renewed
            .caveats
            .into_iter()
            .map(|c| {
                if matches!(c, Caveat::BootEpoch(_)) {
                    Caveat::BootEpoch(new_boot)
                } else {
                    c
                }
            })
            .collect(),
    )
    .map_err(|e| format!("renewal: {e:?}"))?;
    let session = renewed
        .verify(
            &KeyedPrf(auth_key),
            &RequestCtx {
                tenant_incarnation: 11,
                session: 1,
                peer_uid: 42,
                audience: 7,
                key_epoch: 1,
                boot_epoch: new_boot,
                op_bit: OP_READ,
                now_ns: clock.now_ns(),
            },
        )
        .map_err(|e| format!("verify: {e:?}"))?;
    let rows = recovered
        .execute(
            &ciagomem_ql::parse("@1 select {credits: state.credits}")
                .map_err(|e| format!("parse: {e:?}"))?,
            &session,
        )
        .map_err(|e| format!("query: {e:?}"))?;
    assert_eq!(rows.rows[0].values["credits"], Scalar::Int(75));
    println!(
        "Durable host: wrote100, mutated to75, checkpointed, restarted with a fresh boot epoch, recovered75."
    );
    drop(recovered);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
