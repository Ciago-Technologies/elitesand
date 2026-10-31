//! ABI sizes on the running target; reader allocations and allocator metadata are extra.
use ciagomem_core::*;
fn main() {
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
