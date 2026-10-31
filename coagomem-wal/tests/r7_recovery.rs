use coagomem_core::{CoreControl, SlotArena, SlotReadError, TenantWord, Visibility};
use coagomem_wal::gclog::*;
#[test]
fn r7_4_r7_6_validate_controls_and_publish_recovered_prefix_after_superblock() {
    let controls = CoreControl::shared(1);
    let arena = SlotArena::<512>::new(0, 1, controls.clone());
    let second = SlotArena::<512>::new(1, 1, controls.clone());
    let unrelated = SlotArena::<512>::new(2, 1, CoreControl::shared(1));
    let cfg = LogConfig {
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
    };
    // Trusted recovery result: committed suffix ends at 5, already applied to this slot.
    let rec = Recovery {
        next_lsn: 6,
        ..Recovery::fresh()
    };
    let (mut log, sb) = GroupCommitLog::open(cfg, &rec, controls, 0);
    assert_eq!(log.validate_arena(&arena), Ok(()));
    assert_eq!(log.validate_arena(&second), Ok(()));
    assert_eq!(
        log.validate_arena(&unrelated),
        Err(LogError::MismatchedControls)
    );
    let handle = arena
        .install(
            0,
            TenantWord {
                tenant: 1,
                core: 0,
                layout_version: 1,
                flags: 0,
            },
            123,
            5,
            5,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        arena.read(handle, 1, 123, Visibility::Durable, |_| Some(())),
        Err(SlotReadError::NotYetDurable { .. })
    ));
    log.superblock_durable(&sb);
    assert_eq!(arena.core(0).durable(), 5);
    assert!(
        arena
            .read(handle, 1, 123, Visibility::Durable, |_| Some(()))
            .is_ok()
    );
}
