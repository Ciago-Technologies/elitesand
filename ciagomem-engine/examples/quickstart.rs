//! A complete local insert/update/read example using the embedded engine.
use ciagomem_core::{
    Caveat, Clock, KeyedPrf, Label, Macaroon, MonotonicClock, OP_READ, OP_WRITE, RequestCtx,
    SessionEpoch, random_id,
};
use ciagomem_engine::{ActorRef, Engine, Error, Limits, MemoryJournal, Record, Scalar};
use ciagomem_ql::parse;
use std::sync::Arc;

fn main() -> ciagomem_engine::Result<()> {
    let clock = Arc::new(MonotonicClock);
    let boot = SessionEpoch::new().map_err(|_| Error::Permission)?.id();
    let tenant = random_id().map_err(|_| Error::Permission)?;
    let mut secret = [0; 32];
    for chunk in secret.chunks_exact_mut(16) {
        chunk.copy_from_slice(&random_id().map_err(|_| Error::Permission)?.to_le_bytes());
    }
    let key = KeyedPrf(secret);
    let now = clock.now_ns();
    // This is an in-process demo; a network adapter must authenticate real peer credentials.
    let peer_uid = 1000;
    let session = Macaroon::issue(
        &key,
        vec![
            Caveat::Tenant(tenant),
            Caveat::Session(1),
            Caveat::BootEpoch(boot),
            Caveat::Audience(7),
            Caveat::PeerUid(peer_uid),
            Caveat::Ops(OP_READ | OP_WRITE),
            Caveat::KeyEpoch(1),
            Caveat::ExpiresAt(now.saturating_add(60_000_000_000)),
        ],
    )
    .map_err(|_| Error::Permission)?
    .verify(
        &key,
        &RequestCtx {
            tenant_incarnation: tenant,
            session: 1,
            boot_epoch: boot,
            audience: 7,
            peer_uid,
            op_bit: OP_READ,
            key_epoch: 1,
            now_ns: now,
        },
    )
    .map_err(|_| Error::Permission)?;

    let mut engine = Engine::new(
        1,
        Limits::default(),
        MemoryJournal::new(8 << 20),
        boot,
        clock,
    )?;
    let actor = ActorRef {
        id: 1,
        incarnation: random_id().map_err(|_| Error::Permission)?,
    };
    let label = Label::external(1, vec![tenant]).map_err(|_| Error::InvalidValue)?;
    engine.put(
        Record::new(
            actor,
            tenant,
            [("credits".into(), Scalar::Int(100))].into(),
            label,
        ),
        &session,
    )?;
    println!("Created actor 1");

    let update = parse("@1 mutate {state.credits -= 25}").map_err(|_| Error::Parse)?;
    engine.execute(&update, &session)?;
    let query = parse("@1 select {credits: state.credits}").map_err(|_| Error::Parse)?;
    let result = engine.execute(&query, &session)?;
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0].values.get("credits"), Some(&Scalar::Int(75)));
    let snapshot = engine.snapshot(actor, &session)?;
    assert_eq!(
        snapshot.record.fields.get("credits"),
        Some(&Scalar::Int(75))
    );
    println!("Credits after update: 75");
    Ok(())
}
