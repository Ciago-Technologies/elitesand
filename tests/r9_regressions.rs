//! R9 acceptance: lifecycle/arena teardown with outstanding device I/O stays sound.
use ciagomem_core::*;
fn lc_cfg() -> lifecycle::LifecycleConfig {
    lifecycle::LifecycleConfig {
        low_free: 0,
        high_free: 1,
        max_flush_inflight: 1,
        max_retries: 2,
        epoch: 2,
    }
}
#[test]
fn r9_1_lifecycle_drop_with_inflight_read_fences_and_arena_memory_survives() {
    use io::{Outcome, SimIo};
    use lifecycle::*;
    use slot::RouteTable;
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut disk = SimIo::new(1, 8 * 4096);
    {
        let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
        lc.admit_load(
            1,
            123,
            RecoveryBounds::clean(2, 1, 5),
            ExtentLoc::new(0, 2, false),
            1,
            false,
            &mut disk,
        )
        .unwrap();
        assert!(!lc.quiescent());
    }
    assert_eq!(arena.io_pending(), 1);
    assert!(arena.core(0).is_fenced());
    drop(arena); // leaked: a device op still targets it
    drop(route);
    let _ = disk.complete(0, Outcome::Ok); // no crash, no UB
}
#[test]
fn r9_1_completed_io_returns_arena_to_zero_pending() {
    use io::{Outcome, SimIo};
    use lifecycle::*;
    use slot::RouteTable;
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut disk = SimIo::new(1, 8 * 4096);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
    lc.admit_load(
        1,
        123,
        RecoveryBounds::clean(2, 1, 5),
        ExtentLoc::new(0, 2, false),
        1,
        false,
        &mut disk,
    )
    .unwrap();
    let mut events = Vec::new();
    while !disk.inflight.is_empty() {
        let (ud, res) = disk.complete(0, Outcome::Ok);
        /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
        unsafe { lc.on_cqe(ud, res, &mut disk, &mut events) };
    }
    assert!(lc.quiescent());
    assert_eq!(arena.io_pending(), 0);
    drop(lc);
    assert!(!arena.core(0).is_fenced());
}
#[test]
fn r9_1_released_lifecycle_neither_fences_nor_leaks() {
    use io::SimIo;
    use lifecycle::*;
    use slot::RouteTable;
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut disk = SimIo::new(1, 8 * 4096);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
    lc.admit_load(
        1,
        123,
        RecoveryBounds::clean(2, 1, 5),
        ExtentLoc::new(0, 2, false),
        1,
        false,
        &mut disk,
    )
    .unwrap();
    disk.inflight.clear(); // the device abandons the op (ring destroyed)
    // SAFETY: no simulated op references the arena any more.
    unsafe { lc.release_device_references() };
    drop(lc);
    assert_eq!(arena.io_pending(), 0);
    assert!(!arena.core(0).is_fenced());
}

mod support;
#[test]
fn r9_1_submission_unwind_keeps_read_and_checkpoint_buffers_live() {
    use io::{Outcome, SimIo};
    use lifecycle::*;
    use slot::RouteTable;
    for read in [true, false] {
        let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
        let route = RouteTable::new(4);
        let mut disk = support::PanicAfterSubmit(SimIo::new(1, 8 * 4096));
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
            if read {
                lc.admit_load(
                    1,
                    123,
                    RecoveryBounds::clean(2, 1, 5),
                    ExtentLoc::new(0, 2, false),
                    1,
                    false,
                    &mut disk,
                )
                .unwrap();
            } else {
                let h = lc
                    .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut Vec::new())
                    .unwrap();
                lc.checkpoint(h, 5, &mut disk);
            }
        }));
        assert!(caught.is_err());
        assert!(arena.core(0).is_fenced());
        assert_eq!(arena.io_pending(), 1);
        drop(arena);
        let _ = disk.0.complete(0, Outcome::Ok);
    }
}

#[test]
fn r9_1_rejected_read_and_checkpoint_roll_back_pending_counts() {
    use lifecycle::*;
    use slot::RouteTable;
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut disk = support::RejectSubmit::default();
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
    assert_eq!(
        lc.admit_load(
            1,
            123,
            RecoveryBounds::clean(2, 1, 5),
            ExtentLoc::new(0, 2, false),
            1,
            false,
            &mut disk
        ),
        Err(AdmitError::SqFull)
    );
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut Vec::new())
        .unwrap();
    assert!(!lc.checkpoint(h, 5, &mut disk));
    assert_eq!(disk.calls, 2);
    assert!(lc.quiescent());
    assert_eq!(arena.io_pending(), 0);
    drop(lc);
    assert!(!arena.core(0).is_fenced());
}
