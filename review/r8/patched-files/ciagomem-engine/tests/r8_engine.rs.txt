//! R8-3 acceptance: the engine stays movable to and shareable with core threads.
fn send_sync<T: Send + Sync>() {}
#[test]
fn r8_3_engine_is_send_and_sync() {
    send_sync::<ciagomem_engine::Engine<ciagomem_engine::MemoryJournal>>();
}
