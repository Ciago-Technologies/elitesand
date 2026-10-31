//! Reproducible local measurements, not a SQL comparison or production SLO claim.
use ciagomem_engine::*;
use coagomem_core::{
    Caveat, Integrity, KeyedPrf, Label, Macaroon, OP_OPERATOR, OP_READ, OP_WRITE, RequestCtx,
};
use coagomem_ql::parse;
use std::{hint::black_box, time::Instant};
fn main() -> Result<()> {
    let key = KeyedPrf([7; 32]);
    let token = Macaroon::issue(
        &key,
        vec![
            Caveat::Tenant(11),
            Caveat::ExpiresAt(u64::MAX),
            Caveat::PeerUid(42),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(1),
            Caveat::Ops(OP_READ | OP_WRITE | OP_OPERATOR),
            Caveat::KeyEpoch(1),
        ],
    )
    .unwrap()
    .verify(
        &key,
        &RequestCtx {
            tenant_incarnation: 11,
            key_epoch: 1,
            now_ns: 1,
            op_bit: OP_READ,
            peer_uid: 42,
            session: 1,
            audience: 7,
            boot_epoch: 1,
        },
    )
    .unwrap();
    let mut engine = Engine::new(
        4,
        Limits::default(),
        MemoryJournal::new(256 << 20),
        1,
        std::sync::Arc::new(coagomem_core::MonotonicClock),
    )?;
    for id in 1..=10_000 {
        engine.put(
            Record::new(
                ActorRef {
                    id,
                    incarnation: id as u128,
                },
                11,
                [("value".into(), Scalar::Int(id as i64))].into(),
                test_label(Integrity::UserDirect, 1, vec![11]).unwrap(),
            ),
            &token,
        )?;
    }
    let q = parse("scan actors[*] where state.value == 777 select {value: state.value}").unwrap();
    for _ in 0..10 {
        black_box(engine.execute(&q, &token)?);
    }
    let mut measures = Vec::new();
    for (name, disabled) in [("bitmap", false), ("scalar", true)] {
        let start = Instant::now();
        let mut work = 0;
        for _ in 0..100 {
            let result = engine.execute_with(
                &q,
                &token,
                Options {
                    disable_indexes: disabled,
                    ..Default::default()
                },
            )?;
            assert_eq!(result.rows.len(), 1);
            work = result.stats.work;
            black_box(result);
        }
        measures.push(format!(
            "\"{name}\":{{\"ns_per_query\":{},\"work_units\":{work}}}",
            start.elapsed().as_nanos() / 100
        ));
    }
    let target = ActorRef {
        id: 777,
        incarnation: 777,
    };
    let start = Instant::now();
    for _ in 0..100_000 {
        black_box(engine.snapshot(target, &token)?);
    }
    println!(
        "{{\"rows\":10000,\"cores_simulated\":4,\"snapshot_ns\":{},\"accounted_bytes\":{},{} }}",
        start.elapsed().as_nanos() / 100_000,
        engine.accounted_bytes(),
        measures.join(",")
    );
    Ok(())
}

fn test_label(
    integrity: Integrity,
    sources: u64,
    readers: Vec<u128>,
) -> std::result::Result<Label, &'static str> {
    if integrity == Integrity::External {
        return Label::external(sources, readers);
    }
    let issuer = coagomem_core::AttestationSigner::from_seed([3; 32]);
    let tenant = readers.first().copied().unwrap_or(11);
    let proof = issuer.provenance(coagomem_core::Provenance {
        tenant,
        session: 1,
        boot_epoch: 1,
        integrity: integrity as u8,
        sources,
        readers,
        digest: coagomem_core::content_digest(b"value"),
        expires_at: 1000,
    });
    Label::attested(&proof, &issuer.verifier(), tenant, 1, 1, b"value", 1)
}
