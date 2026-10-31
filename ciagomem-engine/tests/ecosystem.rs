use ciagomem_engine::*;
use ciagomem_engine::{index::Sq8, links::OnDelete};
use coagomem_core::{
    Caveat, Integrity, KeyedPrf, Label, Macaroon, OP_OPERATOR, OP_READ, OP_WRITE, RequestCtx,
    VerifiedMacaroon,
};
use coagomem_ql::parse;
fn session(tenant: u128) -> VerifiedMacaroon {
    let k = KeyedPrf([7; 32]);
    Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(tenant),
            Caveat::ExpiresAt(1000),
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
        &k,
        &RequestCtx {
            tenant_incarnation: tenant,
            key_epoch: 1,
            now_ns: 1,
            op_bit: OP_READ,
            peer_uid: 42,
            session: 1,
            audience: 7,
            boot_epoch: 1,
        },
    )
    .unwrap()
}
fn actor(id: u64) -> ActorRef {
    ActorRef {
        id,
        incarnation: 1000 + id as u128,
    }
}
fn record(id: u64, tenant: u128, fields: &[(&str, Scalar)]) -> Record {
    Record::new(
        actor(id),
        tenant,
        fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect(),
        test_label(Integrity::UserDirect, 1, vec![tenant]).unwrap(),
    )
}
fn engine() -> Engine<MemoryJournal> {
    Engine::new(2, Limits::default(), MemoryJournal::new(64 << 20)).unwrap()
}
fn run(e: &mut Engine<MemoryJournal>, s: &str) -> QueryResult {
    e.execute(&parse(s).unwrap(), &session(11)).unwrap()
}
#[test]
fn verified_flatbuffer_retains_full_identity_and_borrowed_payload() {
    let mut r = record(1, 11, &[("body", Scalar::Text("hello".into()))]);
    r.actor.incarnation = (1u128 << 100) + 1;
    r.tenant = (1u128 << 90) + 11;
    let bytes = image::encode(&r);
    let v = image::view(&bytes).unwrap();
    assert_eq!(v.incarnation_hi(), (r.actor.incarnation >> 64) as u64);
    assert_eq!(v.tenant_hi(), (r.tenant >> 64) as u64);
    let text = v.fields().get(0).text_value().unwrap();
    assert_eq!(text, "hello");
    let start = bytes.as_ptr() as usize;
    assert!((start..start + bytes.len()).contains(&(text.as_ptr() as usize)));
    for len in 0..bytes.len() {
        if len < 8 {
            assert!(image::view(&bytes[..len]).is_err())
        }
    }
    let mut corrupt = bytes.clone();
    corrupt[..4].fill(255);
    assert!(image::view(&corrupt).is_err());
}
#[test]
fn typed_join_resolves_across_cores_and_propagates_labels() {
    let mut e = engine();
    let t = session(11);
    let mut org = record(2, 11, &[("name", Scalar::Text("Ciago".into()))]);
    org.label = test_label(Integrity::External, 2, vec![11]).unwrap();
    e.put(org, &t).unwrap();
    e.put(record(1, 11, &[("org", Scalar::Ref(actor(2)))]), &t)
        .unwrap();
    assert_ne!(e.owner_core(1), e.owner_core(2));
    let r = run(
        &mut e,
        "@1 join @actor(state.org) as @org where @org.state.name == \"Ciago\" select {organization: @org.state.name}",
    );
    assert_eq!(
        r.rows[0].values["organization"],
        Scalar::Text("Ciago".into())
    );
    assert_eq!(r.rows[0].label.integrity(), Integrity::External);
    assert_eq!(r.rows[0].label.sources(), 3);
}
#[test]
fn foreign_and_stale_join_targets_are_rejected() {
    let mut e = engine();
    e.put(record(2, 22, &[]), &session(22)).unwrap();
    e.put(
        record(1, 11, &[("org", Scalar::Ref(actor(2)))]),
        &session(11),
    )
    .unwrap();
    assert_eq!(
        e.execute(
            &parse("@1 join @actor(state.org) as @org").unwrap(),
            &session(11)
        )
        .unwrap_err(),
        Error::ForeignActor
    );
    e.delete(actor(2), &session(22)).unwrap();
    let mut new = record(2, 11, &[]);
    new.actor.incarnation += 100;
    e.put(new, &session(11)).unwrap();
    assert_eq!(
        e.execute(
            &parse("@1 join @actor(state.org) as @org").unwrap(),
            &session(11)
        )
        .unwrap_err(),
        Error::StaleReference
    );
}
#[test]
fn query_execution_applies_guards_and_publishes_after_validation() {
    let mut e = engine();
    let t = session(11);
    e.put(record(1, 11, &[("wallet", Scalar::Int(100))]), &t)
        .unwrap();
    let r = run(
        &mut e,
        "@1 where state.wallet >= 50 mutate {state.wallet -= 50} select {remaining: state.wallet}",
    );
    assert_eq!(r.rows[0].values["remaining"], Scalar::Int(50));
    assert!(
        run(
            &mut e,
            "@1 where state.wallet > 100 mutate {state.wallet = 0}"
        )
        .rows
        .is_empty()
    );
    let count = e.journal().batches.len();
    let q = parse("@1 mutate {state.wallet -= 1} select {x: frame.unknown}").unwrap();
    assert_eq!(e.execute(&q, &t).unwrap_err(), Error::Unsupported);
    assert_eq!(e.journal().batches.len(), count);
    assert_eq!(
        e.snapshot(actor(1), &t).unwrap().record.fields["wallet"],
        Scalar::Int(50)
    );
}
#[test]
fn restrict_and_atomic_cascade_follow_reverse_edges() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=3 {
        e.put(record(id, 11, &[]), &t).unwrap();
    }
    // Deleting 1 cascades to dependent 2, but dependent 3 restricts deletion of 2.
    e.add_link(actor(2), actor(1), OnDelete::Cascade, &t)
        .unwrap();
    e.add_link(actor(3), actor(2), OnDelete::Restrict, &t)
        .unwrap();
    assert_eq!(e.delete(actor(1), &t), Err(Error::Restricted));
    for id in 1..=3 {
        e.snapshot(actor(id), &t).unwrap();
    }
    e.add_link(actor(3), actor(2), OnDelete::Cascade, &t)
        .unwrap();
    assert_eq!(e.delete(actor(1), &t).unwrap(), 3);
    assert!(matches!(
        e.journal().batches.last().unwrap().changes.as_slice(),
        [Change::Delete(_), Change::Delete(_), Change::Delete(_)]
    ));
}
#[test]
fn cascade_cycles_are_bounded_and_idempotent_stale_refs_fail() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=2 {
        e.put(record(id, 11, &[]), &t).unwrap();
    }
    e.add_link(actor(1), actor(2), OnDelete::Cascade, &t)
        .unwrap();
    e.add_link(actor(2), actor(1), OnDelete::Cascade, &t)
        .unwrap();
    assert_eq!(e.delete(actor(1), &t), Ok(2));
    assert_eq!(e.delete(actor(1), &t), Err(Error::MissingActor));
}
#[test]
fn ciaql_link_requires_ownership_and_has_real_delete_semantics() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=2 {
        e.put(record(id, 11, &[]), &t).unwrap();
    }
    run(&mut e, "@2 link add(target: @1, on_delete: CASCADE)");
    assert_eq!(e.delete(actor(1), &t), Ok(2));
    assert!(parse("@1 join @actor(state.org) as @org select {x: @missing.state.x}").is_err());
    assert!(parse("@1 join @actor(state.org) as @org mutate {@org.state.x=1}").is_err());
}
fn add_plan(fields: &Fields) -> Result<Fields> {
    let mut out = fields.clone();
    out.insert("plan".into(), Scalar::Text("free".into()));
    Ok(out)
}
#[test]
fn schema_projection_is_read_only_and_upgrade_is_journal_gated() {
    let mut e = engine();
    let t = session(11);
    let r = record(1, 11, &[("wallet", Scalar::Int(3))]);
    let original_label = r.label.clone();
    e.put(r, &t).unwrap();
    let before = e.journal().batches.len();
    e.schemas.register(1, add_plan).unwrap();
    let projected = e.projected(actor(1), &t).unwrap();
    assert_eq!(projected.schema_version, 2);
    assert_eq!(projected.label, original_label);
    assert_eq!(e.snapshot(actor(1), &t).unwrap().record.schema_version, 1);
    assert_eq!(e.journal().batches.len(), before);
    assert_eq!(
        run(&mut e, "@1 select {plan: state.plan}").rows[0].values["plan"],
        Scalar::Text("free".into())
    );
    assert!(e.upgrade(actor(1), &t).unwrap());
    assert!(!e.upgrade(actor(1), &t).unwrap());
    let v = e.snapshot(actor(1), &t).unwrap();
    assert_eq!(image::view(&v.image).unwrap().schema_version(), 2);
    assert_eq!(v.record.label, original_label);
}
#[test]
fn indexes_match_scalar_oracle_for_integer_boundaries_and_boolean_logic() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=80 {
        e.put(
            record(
                id,
                11,
                &[
                    ("value", Scalar::Int(id as i64 - 40)),
                    (
                        "region",
                        Scalar::Text(if id % 2 == 0 { "east" } else { "west" }.into()),
                    ),
                ],
            ),
            &t,
        )
        .unwrap();
    }
    for expr in [
        "state.value > 10",
        "state.value >= 0",
        "state.value < -10",
        "state.value <= -10",
        "state.value == 0",
        "state.region == \"east\" AND state.value > 0",
        "state.value == 3 OR state.value == 5",
        "state.value == 3 OR state.region CONTAINS \"es\"",
    ] {
        let source = format!("scan actors[*] where {expr} select {{v: state.value}}");
        let q = parse(&source).unwrap();
        let indexed = e.execute(&q, &t).unwrap();
        let plain = e
            .execute_with(
                &q,
                &t,
                Options {
                    disable_indexes: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            indexed.rows.iter().map(|r| r.actor).collect::<Vec<_>>(),
            plain.rows.iter().map(|r| r.actor).collect::<Vec<_>>(),
            "{expr}"
        );
    }
}
#[test]
fn bitmap_point_filter_visits_only_candidates() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=500 {
        e.put(record(id, 11, &[("v", Scalar::Int(id as i64))]), &t)
            .unwrap();
    }
    let q = parse("scan actors[*] where state.v == 127 select {v: state.v}").unwrap();
    let indexed = e.execute(&q, &t).unwrap();
    let plain = e
        .execute_with(
            &q,
            &t,
            Options {
                disable_indexes: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(indexed.rows[0].values, plain.rows[0].values);
    assert!(indexed.stats.work < plain.stats.work / 10);
    assert_eq!(indexed.stats.bitmap_candidates, 1);
}
#[test]
fn text_search_replaces_postings_and_keeps_tenant_statistics_separate() {
    let mut e = engine();
    let t = session(11);
    e.put(
        record(
            1,
            11,
            &[("body", Scalar::Text("actor memory memory".into()))],
        ),
        &t,
    )
    .unwrap();
    e.put(
        record(2, 11, &[("body", Scalar::Text("cold disk".into()))]),
        &t,
    )
    .unwrap();
    e.put(
        record(3, 22, &[("body", Scalar::Text("secret memory".into()))]),
        &session(22),
    )
    .unwrap();
    let q = "scan actors[*] search(field: body, query: \"memory\", top_k: 10) select {body: state.body}";
    assert_eq!(
        run(&mut e, q)
            .rows
            .iter()
            .map(|r| r.actor.unwrap().id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    e.put(
        record(1, 11, &[("body", Scalar::Text("changed".into()))]),
        &t,
    )
    .unwrap();
    assert!(run(&mut e, q).rows.is_empty());
    e.delete(actor(2), &t).unwrap();
    assert!(
        run(
            &mut e,
            "scan actors[*] search(field: body, query: \"disk\", top_k: 10)"
        )
        .rows
        .is_empty()
    );
}
#[test]
fn hybrid_search_filters_then_ranks_without_crossing_tenants() {
    let mut e = engine();
    let t = session(11);
    for (id, body, v) in [
        (1, "memory actors", vec![1.0, 0.0]),
        (2, "memory disks", vec![0.0, 1.0]),
        (3, "unrelated", vec![1.0, 0.0]),
    ] {
        let mut r = record(
            id,
            11,
            &[
                ("body", Scalar::Text(body.into())),
                ("active", Scalar::Bool(true)),
            ],
        );
        r.embedding = v;
        e.put(r, &t).unwrap();
    }
    let q=parse("scan actors[*] where state.active == true search(field: body, query: \"memory\", top_k: 10) similarity(vector: [1.0,0.0], metric: cosine, threshold: 0.8) select {body: state.body} take 3").unwrap();
    let exact = e.execute(&q, &t).unwrap();
    assert_eq!(exact.rows.len(), 1);
    assert_eq!(exact.rows[0].actor, Some(actor(1)));
    assert!(!exact.stats.approximate);
    let approximate = e
        .execute_with(
            &q,
            &t,
            Options {
                approximate_candidates: Some(10),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(approximate.rows[0].actor, Some(actor(1)));
    assert!(approximate.stats.approximate);
}
#[test]
fn sq8_simd_agrees_with_scalar_quantized_dot_including_tails() {
    for n in [1, 7, 8, 9, 33, 4096] {
        let a: Vec<f32> = (0..n).map(|i| (i as f32 * 0.17).sin()).collect();
        let b: Vec<f32> = (0..n).map(|i| (i as f32 * 0.19).cos()).collect();
        let qa = Sq8::quantize(&a).unwrap();
        let qb = Sq8::quantize(&b).unwrap();
        let ac: Vec<_> = a
            .iter()
            .map(|v| (*v / qa.scale()).round().clamp(-127.0, 127.0))
            .collect();
        let bc: Vec<_> = b
            .iter()
            .map(|v| (*v / qb.scale()).round().clamp(-127.0, 127.0))
            .collect();
        let dot = ac.iter().zip(&bc).map(|(a, b)| a * b).sum::<f32>();
        let norm =
            (ac.iter().map(|v| v * v).sum::<f32>() * bc.iter().map(|v| v * v).sum::<f32>()).sqrt();
        let expected = if norm == 0.0 { 0.0 } else { dot / norm };
        assert!((qa.cosine(&qb).unwrap() - expected).abs() < 1e-5);
    }
    assert!(Sq8::quantize(&[f32::NAN]).is_err());
    assert!(Sq8::quantize(&[]).is_err());
}
#[test]
fn analytics_group_sum_distinct_and_quantile_are_executed() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=5 {
        e.put(
            record(
                id,
                11,
                &[
                    (
                        "region",
                        Scalar::Text(if id <= 3 { "east" } else { "west" }.into()),
                    ),
                    ("amount", Scalar::Int(id as i64)),
                ],
            ),
            &t,
        )
        .unwrap();
    }
    let r = run(
        &mut e,
        "scan actors[*] group by (state.region) aggregate {n: count(), total: sum(state.amount), unique: count_distinct(state.amount), p95: p95(state.amount)} take 2",
    );
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.rows[0].values["region"], Scalar::Text("east".into()));
    assert_eq!(r.rows[0].values["n"], Scalar::Int(3));
    assert_eq!(r.rows[0].values["total"], Scalar::Int(6));
    assert_eq!(r.rows[0].values["p95"], Scalar::Float(3.0));
    assert_eq!(
        run(
            &mut e,
            "scan actors[*] where state.amount > 100 aggregate {n: count()}"
        )
        .rows[0]
            .values["n"],
        Scalar::Int(0)
    );
}
struct FailAfter {
    remaining: usize,
}
impl Journal for FailAfter {
    fn commit(&mut self, _: &Batch) -> Result<()> {
        if self.remaining == 0 {
            Err(Error::Journal)
        } else {
            self.remaining -= 1;
            Ok(())
        }
    }
}
#[test]
fn journal_failure_prevents_state_and_index_publication() {
    let mut e = Engine::new(1, Limits::default(), FailAfter { remaining: 1 }).unwrap();
    let t = session(11);
    e.put(record(1, 11, &[("v", Scalar::Int(1))]), &t).unwrap();
    assert_eq!(
        e.execute(&parse("@1 mutate {state.v=2}").unwrap(), &t)
            .unwrap_err(),
        Error::Journal
    );
    assert_eq!(
        e.snapshot(actor(1), &t).unwrap().record.fields["v"],
        Scalar::Int(1)
    );
}
#[test]
fn all_resource_failures_are_explicit_and_do_not_commit() {
    let t = session(11);
    let mut limits = Limits::default();
    limits.groups = 1;
    limits.cascade_rows = 1;
    let mut e = Engine::new(1, limits, MemoryJournal::new(1 << 20)).unwrap();
    for id in 1..=2 {
        e.put(record(id, 11, &[("g", Scalar::Int(id as i64))]), &t)
            .unwrap();
    }
    assert_eq!(
        e.execute(
            &parse("scan actors[*] group by (state.g) aggregate {n: count()}").unwrap(),
            &t
        )
        .unwrap_err(),
        Error::Budget
    );
    e.add_link(actor(2), actor(1), OnDelete::Cascade, &t)
        .unwrap();
    assert_eq!(e.delete(actor(1), &t), Err(Error::Budget));
    assert!(e.snapshot(actor(1), &t).is_ok());
    let mut limits = Limits::default();
    limits.bytes_per_core = 1;
    let mut e = Engine::new(1, limits, MemoryJournal::new(1 << 20)).unwrap();
    assert_eq!(e.put(record(1, 11, &[]), &t), Err(Error::Budget));
    assert!(e.journal().batches.is_empty());
}
#[test]
fn join_requests_and_arc_snapshots_travel_real_spsc_lanes() {
    use coagomem_core::mesh::Mesh;
    let t = session(11);
    let mut e = engine();
    e.put(record(2, 11, &[("name", Scalar::Text("org".into()))]), &t)
        .unwrap();
    let mut requests = Mesh::<join::Request, 8>::new(2, 2);
    let mut replies = Mesh::<join::Reply, 8>::new(2, 2);
    let mut pending = join::Fanout::new(2);
    let r = pending.begin(11, actor(2), 100).unwrap();
    requests.producers[0][1].try_push(r).unwrap();
    requests.producers[0][1].flush();
    let mut seen = 0;
    requests.consumers[1].poll(8, |source, request| {
        seen += 1;
        replies.producers[1][source]
            .try_push(e.handle_join(&request, &t, 1))
            .unwrap();
        replies.producers[1][source].flush();
    });
    assert_eq!(seen, 1);
    let mut got = None;
    replies.consumers[0].poll(8, |_, reply| {
        got = Some(pending.complete(reply, 2).unwrap())
    });
    assert!(std::sync::Arc::ptr_eq(
        &got.unwrap(),
        &e.snapshot(actor(2), &t).unwrap().record
    ));
    assert_eq!(pending.pending(), 0);
}
#[test]
fn join_tickets_reject_forgery_timeout_and_duplicates() {
    let mut p = join::Fanout::new(1);
    let r = p.begin(11, actor(1), 10).unwrap();
    assert_eq!(p.begin(11, actor(2), 10).unwrap_err(), Error::Budget);
    assert_eq!(
        p.complete(
            join::Reply {
                ticket: r.ticket,
                target: actor(2),
                snapshot: Err(Error::MissingActor)
            },
            1
        )
        .unwrap_err(),
        Error::StaleReference
    );
    assert_eq!(p.pending(), 1);
    assert_eq!(p.expire(10), 1);
    assert_eq!(
        p.complete(
            join::Reply {
                ticket: r.ticket,
                target: actor(1),
                snapshot: Err(Error::MissingActor)
            },
            11
        )
        .unwrap_err(),
        Error::StaleReference
    );
}
#[test]
fn immutable_snapshots_survive_replacement_without_payload_copy() {
    let mut e = engine();
    let t = session(11);
    e.put(record(1, 11, &[("v", Scalar::Int(1))]), &t).unwrap();
    let old = e.snapshot(actor(1), &t).unwrap();
    e.put(record(1, 11, &[("v", Scalar::Int(2))]), &t).unwrap();
    assert_eq!(old.record.fields["v"], Scalar::Int(1));
    assert_eq!(
        image::view(&old.image).unwrap().fields().get(0).int_value(),
        1
    );
}
#[test]
fn readonly_scope_cannot_mutate_or_add_links() {
    let k = KeyedPrf([1; 32]);
    let t = Macaroon::issue(
        &k,
        vec![
            Caveat::Tenant(11),
            Caveat::ExpiresAt(100),
            Caveat::PeerUid(42),
            Caveat::Session(1),
            Caveat::Audience(7),
            Caveat::BootEpoch(1),
            Caveat::Ops(OP_READ),
            Caveat::KeyEpoch(1),
        ],
    )
    .unwrap()
    .verify(
        &k,
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
    let mut e = engine();
    e.put(record(1, 11, &[]), &session(11)).unwrap();
    assert_eq!(
        e.execute(&parse("@1 mutate {state.v=2}").unwrap(), &t)
            .unwrap_err(),
        Error::Permission
    );
    assert_eq!(e.put(record(2, 11, &[]), &t), Err(Error::Permission));
}

#[test]
fn asynchronous_delete_tombstones_before_chunked_cleanup_and_rejects_retries() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=3 {
        e.put(record(id, 11, &[]), &t).unwrap();
    }
    e.add_link(actor(2), actor(1), OnDelete::Cascade, &t)
        .unwrap();
    let status = e.begin_delete(actor(1), &t).unwrap();
    assert_eq!(status.remaining, 2);
    assert_eq!(e.snapshot(actor(1), &t).unwrap_err(), Error::Restricted);
    assert_eq!(
        e.add_link(actor(3), actor(1), OnDelete::Restrict, &t),
        Err(Error::Restricted)
    );
    assert_eq!(run(&mut e, "scan actors[*]").rows.len(), 1);
    let next = e.advance_delete(status.job, 0, 1, &t).unwrap();
    assert_eq!(next.remaining, 1);
    assert_eq!(
        e.advance_delete(status.job, 0, 1, &t),
        Err(Error::StaleReference)
    );
    assert_eq!(
        e.advance_delete(status.job, next.cursor, 1, &t)
            .unwrap()
            .remaining,
        0
    );
    assert!(e.snapshot(actor(3), &t).is_ok());
}

#[test]
fn post_mutation_search_is_rejected_before_any_commit() {
    let mut e = engine();
    let t = session(11);
    e.put(record(1, 11, &[("body", Scalar::Text("old".into()))]), &t)
        .unwrap();
    let before = e.journal().batches.len();
    let q = parse("@1 mutate {state.body=\"new\"} search(field: body, query: \"old\", top_k: 1)")
        .unwrap();
    assert_eq!(e.execute(&q, &t).unwrap_err(), Error::Unsupported);
    assert_eq!(e.journal().batches.len(), before);
}

#[test]
fn range_zone_maps_stay_correct_after_updates_deletion_and_slot_reuse() {
    let mut e = engine();
    let t = session(11);
    for id in 1..=150 {
        e.put(record(id, 11, &[("v", Scalar::Int(id as i64))]), &t)
            .unwrap();
    }
    e.put(record(2, 11, &[("v", Scalar::Int(1000))]), &t)
        .unwrap();
    e.delete(actor(150), &t).unwrap();
    e.put(record(200, 11, &[("v", Scalar::Int(-1000))]), &t)
        .unwrap();
    for query in [
        "scan actors[*] where state.v > 149",
        "scan actors[*] where state.v < -999",
        "scan actors[*] where state.v >= 1000",
    ] {
        let q = parse(query).unwrap();
        let a = e.execute(&q, &t).unwrap();
        let b = e
            .execute_with(
                &q,
                &t,
                Options {
                    disable_indexes: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            a.rows.iter().map(|r| r.actor).collect::<Vec<_>>(),
            b.rows.iter().map(|r| r.actor).collect::<Vec<_>>()
        );
    }
}

#[test]
fn query_work_and_projection_budgets_fail_before_writes() {
    let t = session(11);
    let mut limits = Limits::default();
    limits.query_work = 3;
    let mut e = Engine::new(1, limits, MemoryJournal::new(1 << 20)).unwrap();
    for id in 1..=5 {
        e.put(record(id, 11, &[("v", Scalar::Int(id as i64))]), &t)
            .unwrap();
    }
    assert_eq!(
        e.execute(&parse("scan actors[*] where state.v > 1").unwrap(), &t)
            .unwrap_err(),
        Error::Budget
    );
    let mut limits = Limits::default();
    limits.result_bytes = 1;
    let mut e = Engine::new(1, limits, MemoryJournal::new(1 << 20)).unwrap();
    e.put(record(1, 11, &[("v", Scalar::Int(1))]), &t).unwrap();
    let before = e.journal().batches.len();
    assert_eq!(
        e.execute(&parse("@1 mutate {state.v=2}").unwrap(), &t)
            .unwrap_err(),
        Error::Budget
    );
    assert_eq!(e.journal().batches.len(), before);
}

#[test]
fn mixed_numeric_bitmap_equality_is_exact_above_f64_integer_precision() {
    let t = session(11);
    let mut e = engine();
    e.put(
        record(1, 11, &[("v", Scalar::Int(9_007_199_254_740_993))]),
        &t,
    )
    .unwrap();
    e.put(
        record(2, 11, &[("v", Scalar::Float(9_007_199_254_740_992.0))]),
        &t,
    )
    .unwrap();
    e.put(
        record(3, 11, &[("v", Scalar::Float(9_007_199_254_740_994.0))]),
        &t,
    )
    .unwrap();
    for query in [
        "scan actors[*] where state.v == 9007199254740993",
        "scan actors[*] where state.v == 9007199254740994",
        "scan actors[*] where state.v > 9007199254740992",
    ] {
        let q = parse(query).unwrap();
        let a = e.execute(&q, &t).unwrap();
        let b = e
            .execute_with(
                &q,
                &t,
                Options {
                    disable_indexes: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            a.rows.iter().map(|r| r.actor).collect::<Vec<_>>(),
            b.rows.iter().map(|r| r.actor).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        Scalar::Int(i64::MAX)
            .compare(&Scalar::Float(i64::MAX as f64))
            .unwrap(),
        Some(std::cmp::Ordering::Less)
    );
}

#[test]
fn general_equi_join_is_bounded_authorized_and_preserves_duplicate_matches() {
    let t = session(11);
    let mut e = engine();
    for id in 1..=3 {
        e.put(record(id, 11, &[("key", Scalar::Int(7))]), &t)
            .unwrap();
    }
    let a = e.snapshot(actor(1), &t).unwrap();
    let bs = [
        e.snapshot(actor(2), &t).unwrap(),
        e.snapshot(actor(3), &t).unwrap(),
    ];
    let result = join::equi_join(
        std::slice::from_ref(&a),
        &bs,
        "key",
        "key",
        &t,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(result.len(), 2);
    assert!(std::sync::Arc::ptr_eq(&result[0].left.record, &a.record));
    let mut limits = Limits::default();
    limits.join_rows = 1;
    assert_eq!(
        join::equi_join(&[a], &bs, "key", "key", &t, &limits).unwrap_err(),
        Error::Budget
    );
    assert_eq!(
        join::equi_join(&[], &bs, "key", "key", &session(22), &Limits::default()).unwrap_err(),
        Error::ForeignActor
    );
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
