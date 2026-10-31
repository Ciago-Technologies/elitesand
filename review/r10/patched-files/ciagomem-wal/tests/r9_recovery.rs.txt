//! R9 acceptance: dropping a log never frees a buffer the device still references, and
//! the durable watermark has exactly one publisher.
use ciagomem_core::CoreControl;
use ciagomem_core::io::{Outcome, SimIo};
use ciagomem_core::slot::DurabilityPublisher;
use ciagomem_wal::envelope::KIND_DATA;
use ciagomem_wal::gclog::*;
fn cfg() -> LogConfig {
    LogConfig {
        block: 4096,
        buf_bytes: 16384,
        buffers: 4,
        max_inflight: 3,
        min_batch_bytes: 8192,
        max_delay_ns: 5000,
        capacity: 65536,
        file: 0,
        tail: TailMode::Pad,
        max_retries: 3,
    }
}
fn io() -> SimIo {
    SimIo::new(1, (cfg().data_offset() + cfg().capacity) as usize)
}
#[test]
fn r9_1_dropping_log_with_inflight_write_fences_and_keeps_buffer_live() {
    let controls = CoreControl::shared(1);
    let mut d = io();
    let (mut a, sb) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
    a.superblock_durable(&sb);
    let before = controls[0].durable();
    a.append(1, KIND_DATA, b"payload", Some(1), 0, &mut d)
        .unwrap();
    a.poll(1_000_000, true, &mut d);
    assert!(!a.quiescent());
    drop(a);
    assert!(controls[0].is_fenced());
    // Reopen on the same controls is refused; fresh controls are required.
    let c2 = controls.clone();
    assert!(
        std::panic::catch_unwind(move || {
            GroupCommitLog::open(cfg(), &Recovery::fresh(), c2, 0);
        })
        .is_err()
    );
    // The late completion reads still-live (leaked) memory and publishes nothing.
    let _ = d.complete(0, Outcome::Ok);
    assert_eq!(controls[0].durable(), before);
}
#[test]
fn r9_1_quiescent_drop_still_releases_cleanly() {
    let controls = CoreControl::shared(1);
    let mut d = io();
    let (mut a, sb) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
    a.superblock_durable(&sb);
    a.append(1, KIND_DATA, b"x", Some(1), 0, &mut d).unwrap();
    a.poll(1_000_000, true, &mut d);
    let (ud, res) = d.complete(0, Outcome::Ok);
    /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */
    unsafe { a.on_cqe(ud, res, &mut d, &mut Vec::new(), &mut Vec::new()) }.unwrap();
    assert!(a.quiescent());
    let published = controls[0].durable();
    drop(a);
    assert!(!controls[0].is_fenced() && !controls[0].log_bound());
    let rec = Recovery {
        next_lsn: published + 1,
        ..Recovery::fresh()
    };
    let (_b, _) = GroupCommitLog::open(cfg(), &rec, controls, 0);
}
#[test]
fn r9_2_no_second_publisher_while_a_log_is_bound() {
    let controls = CoreControl::shared(1);
    let (a, _) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
    assert!(controls[0].log_bound());
    assert!(DurabilityPublisher::claim(controls.clone(), 0).is_none());
    // A refused claim must not release the existing one.
    assert!(controls[0].log_bound());
    assert!(DurabilityPublisher::claim(controls.clone(), 1).is_none());
    drop(a);
    assert!(DurabilityPublisher::claim(controls, 0).is_some());
}

#[test]
fn r9_1_new_submission_cannot_reuse_an_earlier_device_release() {
    let controls = CoreControl::shared(1);
    let mut d = io();
    let (mut log, sb) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
    log.superblock_durable(&sb);
    log.append(1, KIND_DATA, b"abandoned", None, 0, &mut d)
        .unwrap();
    log.poll(1_000_000, true, &mut d);
    d.abandon_all();
    // SAFETY: all previously submitted simulator operations have been discarded.
    unsafe { log.release_device_references() };
    log.append(1, KIND_DATA, b"new operation", None, 2_000_000, &mut d)
        .unwrap();
    log.poll(3_000_000, true, &mut d);
    assert_eq!(d.inflight().len(), 1);
    drop(log);
    assert!(
        controls[0].is_fenced(),
        "a release cannot cover I/O submitted afterward"
    );
    let _ = d.complete(0, Outcome::Ok);
}

#[path = "../../tests/support/mod.rs"]
mod support;
#[test]
fn r9_1_submission_unwind_fences_and_keeps_log_buffer_live() {
    let controls = CoreControl::shared(1);
    let mut disk = support::PanicAfterSubmit(io());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (mut log, sb) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
        log.superblock_durable(&sb);
        log.append(1, KIND_DATA, b"submitted", None, 0, &mut disk)
            .unwrap();
        log.poll(1_000_000, true, &mut disk);
    }));
    assert!(caught.is_err());
    assert!(controls[0].is_fenced());
    let _ = disk.0.complete(0, Outcome::Ok);
}

#[test]
fn r9_1_rejected_submission_restores_teardown_state() {
    for released in [false, true] {
        let controls = CoreControl::shared(1);
        let (mut log, sb) = GroupCommitLog::open(cfg(), &Recovery::fresh(), controls.clone(), 0);
        log.superblock_durable(&sb);
        if released {
            let mut disk = io();
            log.append(1, KIND_DATA, b"cancelled", None, 0, &mut disk)
                .unwrap();
            log.poll(1_000_000, true, &mut disk);
            disk.abandon_all();
            // SAFETY: the simulated device no longer retains any operation.
            unsafe { log.release_device_references() };
        }
        let mut full = support::RejectSubmit::default();
        log.append(1, KIND_DATA, b"not submitted", None, 2_000_000, &mut full)
            .unwrap();
        log.poll(3_000_000, true, &mut full);
        assert!(full.calls > 0);
        drop(log);
        assert!(!controls[0].is_fenced());
        assert!(!controls[0].log_bound());
    }
}
