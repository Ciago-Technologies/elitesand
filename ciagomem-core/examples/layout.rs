//! ABI sizes on the running target; reader allocations and allocator metadata are extra.
use ciagomem_core::*;
fn main() {
    assert_eq!(std::mem::align_of::<SlotHeader>(), 64);
    assert_eq!(
        [
            std::mem::offset_of!(SlotHeader, seq),
            std::mem::offset_of!(SlotHeader, owner),
            std::mem::offset_of!(SlotHeader, tenant),
            std::mem::offset_of!(SlotHeader, applied_lsn),
            std::mem::offset_of!(SlotHeader, checkpoint_lsn),
            std::mem::offset_of!(SlotHeader, payload_len),
            std::mem::offset_of!(SlotHeader, last_generation),
            std::mem::offset_of!(SlotHeader, incarnation_lo),
            std::mem::offset_of!(SlotHeader, incarnation_hi),
            std::mem::offset_of!(SlotHeader, reserved),
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 72],
    );
    println!(
        "SlotHeader alignment={} ExtentImage alignment={}",
        std::mem::align_of::<SlotHeader>(),
        std::mem::align_of::<envelope::ExtentImage>()
    );
    println!(
        "SlotHeader={} Label={} CoreControl={} VaultLoc={} ExtentImage={} RefToken={} FrameValue={}",
        size_of::<SlotHeader>(),
        size_of::<Label>(),
        size_of::<CoreControl>(),
        size_of::<VaultLoc>(),
        size_of::<envelope::ExtentImage>(),
        size_of::<RefToken>(),
        size_of::<FrameValue>()
    );
}
