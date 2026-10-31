//! Self-audit acceptance tests: safe access exclusion and single-use pin ownership.
use ciagomem_core::envelope::ExtentImage;
use ciagomem_core::io::{Outcome, SimIo};
use ciagomem_core::lifecycle::*;
use ciagomem_core::slot::{RouteTable, TenantWord};
use ciagomem_core::*;

fn cfg() -> LifecycleConfig {
    LifecycleConfig {
        low_free: 1,
        high_free: 1,
        max_flush_inflight: 1,
        max_retries: 2,
        epoch: 2,
    }
}
fn complete(lc: &mut SlotLifecycle<'_, 512>, io: &mut SimIo, ev: &mut Vec<Event>) {
    let (ud, res) = io.complete(0, Outcome::Ok);
    // SAFETY: the genuine simulator completion releases every device reference.
    unsafe { lc.on_cqe(ud, res, io, ev) };
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
fn checkpoint_cannot_be_changed_through_public_arena_apis() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let mut ev = Vec::new();
    let mut io = SimIo::new(1, 8 * 4096);
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 4, 1, &mut ev)
        .unwrap();
    lc.mutate(h, 5, |w| w.set(20, 7)).unwrap();
    assert!(lc.checkpoint(h, 5, &mut io));
    assert_eq!(
        arena.write(h, 6, |w| w.set(20, 99)),
        Err(WriteError::WriterBusy)
    );
    assert!(arena.with_unowned_payload(0, |w| w.fill(0)).is_err());
    assert!(arena.revoke(h).is_err());
    assert!(arena.install(0, tw(), 456, 6, 0).is_err());
    assert!(arena.set_checkpoint_lsn(h, 6).is_err());
    assert_eq!(
        arena.read::<()>(h, 1, 123, Visibility::Applied, |_| panic!(
            "DMA-owned payload accessed"
        )),
        Err(SlotReadError::WriterActive)
    );
    complete(&mut lc, &mut io, &mut ev);
    let image = &io.files[0][3 * 4096..4 * 4096];
    assert_eq!(
        ExtentImage::validate(image, 1, 123, 2)
            .unwrap()
            .checkpoint_lsn,
        5
    );
    assert_eq!(u64::from_le_bytes(image[160..168].try_into().unwrap()), 7);
    assert!(matches!(lc.state(0), State::Resident(r) if !r.dirty && r.last_lsn == 5));
    assert!(
        arena.write(h, 6, |_| ()).is_err(),
        "owner must account for every mutation"
    );
    lc.mutate(h, 6, |w| w.set(20, 8)).unwrap();
    assert!(matches!(lc.state(0), State::Resident(r) if r.dirty && r.last_lsn == 6));
}

#[test]
fn pending_cpu_reader_defers_dma_without_blocking_the_reactor() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let mut ev = Vec::new();
    let mut io = SimIo::new(1, 8 * 4096);
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    std::thread::scope(|scope| {
        let (entered, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let arena = &arena;
        let reader = scope.spawn(move || {
            let mut first = true;
            let _ = arena.read(h, 1, 123, Visibility::Applied, |v| {
                if first {
                    first = false;
                    entered.send(()).unwrap();
                    wait.recv().unwrap();
                }
                v.word(20)
            });
        });
        ready.recv().unwrap();
        let queued = lc.checkpoint(h, 5, &mut io);
        release.send(()).unwrap();
        reader.join().unwrap();
        assert!(!queued);
        assert!(io.inflight().is_empty());
        assert_eq!(arena.io_pending(), 0);
    });
    assert!(lc.checkpoint(h, 5, &mut io));
    complete(&mut lc, &mut io, &mut ev);
}

#[test]
fn loads_and_abandoned_io_cannot_be_reclaimed_by_another_owner() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let mut io = SimIo::new(1, 8 * 4096);
    lc.admit_load(
        1,
        123,
        RecoveryBounds::clean(2, 1, 5),
        ExtentLoc::new(0, 2, false),
        1,
        false,
        &mut io,
    )
    .unwrap();
    assert!(arena.with_unowned_payload(0, |w| w.fill(1)).is_err());
    assert!(arena.install(0, tw(), 123, 5, 0).is_err());
    let old = SlotHandle::pack(0, 0, 1).unwrap();
    assert_eq!(
        arena.read::<()>(old, 1, 123, Visibility::Applied, |_| panic!(
            "loading buffer accessed"
        )),
        Err(SlotReadError::Reassigned)
    );
    drop(lc);
    assert!(
        std::panic::catch_unwind(|| SlotLifecycle::new(&arena, &route, 0, 0..1, cfg())).is_err()
    );
    assert!(arena.with_unowned_payload(0, |w| w.fill(1)).is_err());
    // Complete through the backend only: the dropped owner's claim must stay closed.
    io.complete(0, Outcome::Ok);
    assert_eq!(arena.io_pending(), 1);
}

#[test]
fn range_claim_is_exclusive_and_failed_claim_rolls_back() {
    let arena = SlotArena::<512>::new_heap(0, 3, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let owner = SlotLifecycle::new(&arena, &route, 0, 1..2, cfg());
    assert!(
        std::panic::catch_unwind(|| SlotLifecycle::new(&arena, &route, 0, 0..2, cfg())).is_err()
    );
    let separate = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    assert!(arena.with_unowned_payload(0, |_| ()).is_err());
    drop(separate);
    arena.with_unowned_payload(0, |_| ()).unwrap();
    drop(owner);
    let _all = SlotLifecycle::new(&arena, &route, 0, 0..3, cfg());
}

#[test]
fn pin_saturation_never_releases_live_slots() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let mut ev = Vec::new();
    let mut io = SimIo::new(1, 8 * 4096);
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    assert!(lc.checkpoint(h, 5, &mut io));
    complete(&mut lc, &mut io, &mut ev);
    let mut pins: Vec<_> = (0..u16::MAX).map(|_| lc.pin(h).unwrap()).collect();
    assert!(lc.pin(h).is_none());
    lc.run_eviction(5, &mut io, &mut ev);
    assert_eq!(lc.free_slots(), 0);
    let last = pins.pop().unwrap();
    for pin in pins {
        lc.unpin(pin, &mut ev).unwrap();
    }
    lc.run_eviction(5, &mut io, &mut ev);
    assert_eq!(lc.free_slots(), 0);
    lc.unpin(last, &mut ev).unwrap();
    lc.run_eviction(5, &mut io, &mut ev);
    lc.run_eviction(5, &mut io, &mut ev);
    assert_eq!(lc.free_slots(), 1);
}

#[test]
fn wrong_lifecycle_cannot_consume_a_pin_token() {
    let a = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let b = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let ra = RouteTable::new(4);
    let rb = RouteTable::new(4);
    let mut x = SlotLifecycle::new(&a, &ra, 0, 0..1, cfg());
    let mut y = SlotLifecycle::new(&b, &rb, 0, 0..1, cfg());
    let mut ev = Vec::new();
    let h = x
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    let other = y
        .admit_new(1, 456, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    assert_eq!(
        h, other,
        "identical handles in different arenas still have different pin owners"
    );
    let pin = x.pin(h).unwrap();
    let own = y.pin(other).unwrap();
    let returned = y.unpin(pin, &mut ev).unwrap_err();
    x.unpin(returned, &mut ev).unwrap();
    y.unpin(own, &mut ev).unwrap();
}

#[test]
fn dropped_owner_with_pins_retains_its_claim_and_fences_the_core() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut Vec::new())
        .unwrap();
    let token = lc.pin(h).unwrap();
    drop(lc);
    assert!(arena.core(0).is_fenced());
    assert!(arena.write(h, 6, |_| ()).is_err());
    assert!(
        std::panic::catch_unwind(|| SlotLifecycle::new(&arena, &route, 0, 0..1, cfg())).is_err()
    );
    drop(token); // Losing the token cannot stand in for an egress notification.
    assert!(arena.with_unowned_payload(0, |_| ()).is_err());
}

#[test]
fn unmanaged_access_still_checks_ownership_and_reader_unwind_releases_guard() {
    let arena = SlotArena::<512>::new_heap(0, 1, CoreControl::shared(1));
    let old = arena.install(0, tw(), 123, 1, 0).unwrap().unwrap();
    arena.revoke(old).unwrap();
    let new = arena.install(0, tw(), 456, 2, 0).unwrap().unwrap();
    assert!(arena.set_checkpoint_lsn(old, 99).is_err());
    assert!(arena.revoke(old).is_err());
    assert!(arena.with_unowned_payload(0, |w| w.fill(0)).is_err());
    assert!(
        std::panic::catch_unwind(|| {
            let _ = arena.read::<()>(new, 1, 456, Visibility::Applied, |_| {
                panic!("reader unwind")
            });
        })
        .is_err()
    );
    arena.revoke(new).unwrap();
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..1, cfg());
    let mut ev = Vec::new();
    let mut io = SimIo::new(1, 8 * 4096);
    let h = lc
        .admit_new(1, 789, ExtentLoc::new(0, 2, false), 3, 1, &mut ev)
        .unwrap();
    assert!(lc.checkpoint(h, 3, &mut io));
    complete(&mut lc, &mut io, &mut ev);
}
