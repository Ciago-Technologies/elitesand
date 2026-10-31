use coagomem_core::io::{Outcome, SimIo};
use coagomem_core::slot::{CoreControl, SlotArena, TenantWord, Visibility};
use coagomem_wal::envelope::KIND_DATA;
use coagomem_wal::gclog::*;

#[test]
fn r5_w1_fence_is_indeterminate_and_late_completion_cannot_publish() {
    let cfg = LogConfig {
        block: 4096,
        buf_bytes: 16 * 1024,
        buffers: 4,
        max_inflight: 3,
        min_batch_bytes: 8 * 1024,
        max_delay_ns: 5_000,
        capacity: 64 * 1024,
        file: 0,
        tail: TailMode::Pad,
        max_retries: 3,
    };
    let mut io = SimIo::new(1, (cfg.data_offset() + cfg.capacity) as usize);
    let controls = CoreControl::shared(2);
    let class0 = SlotArena::<512>::new(0, 1, controls.clone());
    let class1 = SlotArena::<512>::new(1, 1, controls);
    let tw = TenantWord {
        tenant: 1,
        core: 0,
        layout_version: 1,
        flags: 0,
    };
    let h0 = class0.install(0, tw, 123, 1, 0).unwrap().unwrap();
    let h1 = class1.install(0, tw, 123, 1, 0).unwrap().unwrap();
    let core = class0.core(0);
    let (mut log, sb) = GroupCommitLog::open(cfg, &Recovery::fresh(), class0.controls().clone(), 0);
    log.superblock_durable(&sb);
    log.append(1, KIND_DATA, b"a", Some(10), 0, &mut io)
        .unwrap();
    log.poll(1_000_000, true, &mut io);
    log.append(1, KIND_DATA, b"b", Some(11), 0, &mut io)
        .unwrap();
    log.poll(1_000_000, false, &mut io);
    assert_eq!(io.inflight.len(), 2, "two batches in flight");
    let (mut acks, mut failed) = (Vec::new(), Vec::new());
    let mut fenced = false;
    for _ in 0..=cfg.max_retries {
        let i = io.inflight.len() - 1; // batch B (retries are resubmitted at the end)
        let (ud, res) = io.complete(i, Outcome::Err(libc::EIO));
        fenced |= /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */ unsafe {log
            .on_cqe(ud, res, &mut io, &mut acks, &mut failed)}
            .is_err();
    }
    assert!(fenced);
    failed.sort();
    assert_eq!(
        failed,
        vec![AckOutcome::Indeterminate(10), AckOutcome::Indeterminate(11)],
        "both outcomes are indeterminate"
    );
    // Batch A was healthy and completes after the fence.
    let (ud, res) = io.complete(0, Outcome::Ok);
    let r = /* SAFETY: an actual simulator completion or a provably nonmatching stale token. */ unsafe {log.on_cqe(ud, res, &mut io, &mut acks, &mut failed)};
    assert!(r.is_err(), "late completion remains fenced");
    assert!(core.is_fenced());
    assert_eq!(core.durable(), 0);
    assert!(acks.is_empty());
    assert!(
        class0
            .read(h0, 1, 123, Visibility::Applied, |_| Some(()))
            .is_err()
    );
    assert!(
        class1
            .read(h1, 1, 123, Visibility::Applied, |_| Some(()))
            .is_err()
    );
}
