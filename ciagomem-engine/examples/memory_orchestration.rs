//! Deterministic worker handoff; no model/API/filesystem calls and no durability claim.
use ciagomem_core::*;
use ciagomem_engine::{memory::*, *};
use std::sync::Arc;
fn session(key: &KeyedPrf, tenant: u128, id: u128, boot: u128, now: u64) -> VerifiedMacaroon {
    Macaroon::issue(
        key,
        vec![
            Caveat::Tenant(tenant),
            Caveat::Session(id),
            Caveat::BootEpoch(boot),
            Caveat::Audience(7),
            Caveat::PeerUid(1000),
            Caveat::Ops(OP_READ | OP_WRITE),
            Caveat::KeyEpoch(1),
            Caveat::ExpiresAt(now + 60_000_000_000),
        ],
    )
    .unwrap()
    .verify(
        key,
        &RequestCtx {
            tenant_incarnation: tenant,
            session: id,
            boot_epoch: boot,
            audience: 7,
            peer_uid: 1000,
            op_bit: OP_READ,
            key_epoch: 1,
            now_ns: now,
        },
    )
    .unwrap()
}
fn main() -> ciagomem_engine::Result<()> {
    let clock = Arc::new(MonotonicClock);
    let boot = SessionEpoch::new().map_err(|_| Error::Permission)?.id();
    let tenant = random_id().map_err(|_| Error::Permission)?;
    // Trusted host issuance only. Never give worker processes this signing material.
    let mut secret = [0u8; 32];
    for chunk in secret.chunks_exact_mut(16) {
        chunk.copy_from_slice(&random_id().map_err(|_| Error::Permission)?.to_le_bytes());
    }
    let key = KeyedPrf(secret);
    let now = clock.now_ns();
    let controller = session(&key, tenant, 1, boot, now);
    let worker = session(&key, tenant, 2, boot, now);
    let engine = Engine::new(
        1,
        Limits::default(),
        MemoryJournal::new(8 << 20),
        boot,
        clock,
    )?;
    let mut broker = MemoryBroker::new(engine, &controller, MemoryLimits::default())?;
    let source = ActorRef {
        id: 1,
        incarnation: random_id().map_err(|_| Error::Permission)?,
    };
    let output = ActorRef {
        id: 2,
        incarnation: random_id().map_err(|_| Error::Permission)?,
    };
    let raw = format!(
        "{}ERROR auth: expired session rejected\n",
        "INFO heartbeat normal\n".repeat(1000)
    );
    let raw_bytes = raw.len();
    broker.ingest(
        Record::new(
            source,
            tenant,
            [("body".into(), Scalar::Text(raw))].into(),
            Label::external(1, vec![tenant]).map_err(|_| Error::InvalidValue)?,
        ),
        &controller,
    )?;
    let lease = broker.delegate(output, &[source], &worker, &controller)?;
    // Worker-only input. A sandboxed lightweight model can replace this deterministic
    // extraction, under host token/tool budgets, without exposing raw logs to the main model.
    let inputs = broker.read_sources(&lease, &worker)?;
    let Scalar::Text(body) = inputs[0].record.fields.get("body").unwrap() else {
        return Err(Error::Type);
    };
    let summary = body
        .lines()
        .filter(|line| line.starts_with("ERROR"))
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    let memory = broker.publish(&lease, &summary, &worker)?;
    let context = broker.read_context(&[memory], 1024, &controller)?;
    assert_eq!(context.label.integrity(), Integrity::External);
    assert!(context.text.len() <= 1024);
    assert!(!context.text.contains("INFO heartbeat"));
    println!("Worker source bytes: {raw_bytes}");
    println!(
        "Main context bytes including citations: {}",
        context.text.len()
    );
    println!(
        "Integrity: {:?}; citations: {}",
        context.label.integrity(),
        context.citations.len()
    );
    println!("{}", context.text);
    println!("Byte counts are not measured model tokens, cost savings or latency.");
    Ok(())
}
