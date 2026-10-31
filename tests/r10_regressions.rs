//! R10 acceptance: checkpoint submission unwind, simulator pointer ownership and
//! wake-token typing.
use ciagomem_core::io::{Op, Outcome, SimIo, UserData};
use ciagomem_core::lifecycle::*;
use ciagomem_core::slot::RouteTable;
use ciagomem_core::*;
mod support;
fn lc_cfg() -> LifecycleConfig {
    LifecycleConfig {
        low_free: 0,
        high_free: 1,
        max_flush_inflight: 1,
        max_retries: 2,
        epoch: 2,
    }
}
#[test]
fn r10_1_checkpoint_unwind_keeps_slot_frozen_and_its_completion_matching() {
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
    let mut ev = Vec::new();
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    let mut disk = support::PanicAfterSubmit(SimIo::new(1, 8 * 4096));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lc.checkpoint(h, 5, &mut disk);
    }));
    assert!(caught.is_err());
    let mut io = disk.0;
    assert_eq!(io.inflight().len(), 1);
    assert!(
        matches!(lc.state(h.index()), State::Flushing { .. }),
        "slot stays frozen"
    );
    assert_eq!(lc.flushing(), 1);
    assert!(
        lc.mutate(h, 6, |w| w.set(20, 1)).is_err(),
        "CPU writes are deferred"
    );
    assert!(
        !lc.checkpoint(h, 6, &mut io),
        "no second write of the same payload"
    );
    let (ud, res) = io.complete(0, Outcome::Ok);
    // SAFETY: a genuine simulator completion.
    unsafe { lc.on_cqe(ud, res, &mut io, &mut ev) };
    assert!(
        ev.iter()
            .any(|e| matches!(e, Event::Checkpointed { lsn: 5, .. }))
    );
    assert_eq!(lc.stats.stale_cqes, 0);
    assert!(lc.quiescent());
    assert_eq!(arena.io_pending(), 0);
    drop(lc);
    assert!(!arena.core(0).is_fenced());
}
#[test]
fn r10_1_rejected_checkpoint_restores_resident_state_and_can_retry() {
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
    let mut ev = Vec::new();
    let h = lc
        .admit_new(1, 123, ExtentLoc::new(0, 2, false), 5, 1, &mut ev)
        .unwrap();
    let mut full = support::RejectSubmit::default();
    assert!(!lc.checkpoint(h, 5, &mut full));
    assert!(matches!(lc.state(h.index()), State::Resident(_)));
    assert_eq!(lc.flushing(), 0);
    assert!(lc.mutate(h, 6, |w| w.set(20, 2)).is_ok());
    let mut io = SimIo::new(1, 8 * 4096);
    assert!(lc.checkpoint(h, 6, &mut io));
    let (ud, res) = io.complete(0, Outcome::Ok);
    // SAFETY: a genuine simulator completion.
    unsafe { lc.on_cqe(ud, res, &mut io, &mut ev) };
    assert!(lc.quiescent() && arena.io_pending() == 0);
}
#[test]
fn r10_2_simulator_ops_come_only_from_unsafe_submissions() {
    let mut io = SimIo::new(1, 8 * 4096);
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
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
    assert_eq!(io.inflight().len(), 1);
    io.abandon_all();
    assert!(io.inflight().is_empty());
    // SAFETY: the simulated ring was destroyed; no op references the arena.
    unsafe { lc.release_device_references() };
}
#[cfg(target_os = "linux")]
fn scratch(bytes: u64) -> std::fs::File {
    let p = std::env::temp_dir().join(format!("r10c-{}-{bytes}", std::process::id()));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&p)
        .unwrap();
    f.set_len(bytes).unwrap();
    let _ = std::fs::remove_file(&p);
    f
}
#[cfg(target_os = "linux")]
#[test]
fn r10_4_post_wake_only_posts_wake_tokens() {
    use ciagomem_core::uring::UringIo;
    let (Ok(mut target), Ok(mut sender)) = (UringIo::new(8), UringIo::new(8)) else {
        eprintln!("SKIPPED: io_uring unavailable");
        return;
    };
    sender.post_wake(target.ring_fd(), 7).unwrap();
    sender.submit().unwrap();
    target.submit_and_wait(1).unwrap();
    let mut got = Vec::new();
    target.reap(|ud, res| got.push((ud, res)));
    assert_eq!(got.len(), 1);
    assert_eq!(
        UserData::decode(got[0].0),
        Some(UserData {
            op: Op::Wake,
            object: 0,
            ticket: 7
        })
    );
}
#[cfg(target_os = "linux")]
#[test]
fn r10_uring_lifecycle_drop_before_kernel_submission_reads_into_retained_arena() {
    use ciagomem_core::uring::UringIo;
    use std::os::fd::AsRawFd;
    let Ok(mut ring) = UringIo::new(8) else {
        eprintln!("SKIPPED: io_uring unavailable");
        return;
    };
    let f = scratch(8 * 4096);
    ring.register_files(&[f.as_raw_fd()]).unwrap();
    let arena = SlotArena::<512>::new_heap(0, 2, CoreControl::shared(1));
    let route = RouteTable::new(4);
    {
        let mut lc = SlotLifecycle::new(&arena, &route, 0, 0..2, lc_cfg());
        lc.admit_load(
            1,
            123,
            RecoveryBounds::clean(2, 1, 5),
            ExtentLoc::new(0, 2, false),
            1,
            false,
            &mut ring,
        )
        .unwrap();
    }
    assert!(arena.core(0).is_fenced());
    assert_eq!(arena.io_pending(), 1);
    drop(arena);
    drop(route);
    ring.submit_and_wait(1).unwrap();
    let mut got = Vec::new();
    ring.reap(|ud, res| got.push((ud, res)));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].1, 4096, "kernel read completed into retained memory");
}
