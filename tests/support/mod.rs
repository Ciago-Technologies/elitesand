#![allow(dead_code)]
use coagomem_core::*;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Anchor(Mutex<HashMap<[u8; 16], IdentityHead>>);
impl IdentityAnchor for Anchor {
    fn initialize(&self, id: [u8; 16]) -> std::io::Result<()> {
        let mut m = self.0.lock().unwrap();
        if m.contains_key(&id) {
            return Err(std::io::Error::other("domain exists"));
        }
        m.insert(id, IdentityHead::default());
        Ok(())
    }
    fn head(&self, id: [u8; 16]) -> std::io::Result<IdentityHead> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .copied()
            .ok_or_else(|| std::io::Error::other("unknown anchor domain"))
    }
    fn advance(
        &self,
        id: [u8; 16],
        expected: IdentityHead,
        next: IdentityHead,
    ) -> std::io::Result<()> {
        let mut m = self.0.lock().unwrap();
        if m.get(&id) != Some(&expected) {
            return Err(std::io::Error::other("anchor conflict"));
        }
        m.insert(id, next);
        Ok(())
    }
}
pub fn anchor() -> Arc<dyn IdentityAnchor> {
    Arc::new(Anchor::default())
}
pub fn label(
    integrity: Integrity,
    sources: u64,
    readers: Vec<u128>,
) -> Result<Label, &'static str> {
    if integrity == Integrity::External {
        return Label::external(sources, readers);
    }
    let key = AttestationSigner::from_seed([3; 32]);
    let tenant = readers.first().copied().unwrap_or(11);
    let proof = key.provenance(Provenance {
        tenant,
        session: 1,
        boot_epoch: 1,
        integrity: integrity as u8,
        sources,
        readers,
        digest: content_digest(b"value"),
        expires_at: 1000,
    });
    Label::attested(&proof, &key.verifier(), tenant, 1, 1, b"value", 1)
}

/// Simulates a backend unwinding after it has accepted a buffer reference.
pub struct PanicAfterSubmit(pub coagomem_core::io::SimIo);
impl coagomem_core::io::IoBackend for PanicAfterSubmit {
    unsafe fn write_at(
        &mut self,
        ud: u64,
        file: u16,
        buf: *const u8,
        len: u32,
        offset: u64,
        dsync: bool,
    ) -> Result<(), coagomem_core::io::SqFull> {
        // SAFETY: the caller supplies the same live buffer required by this trait.
        unsafe { self.0.write_at(ud, file, buf, len, offset, dsync)? };
        panic!("simulated unwind after write submission");
    }
    unsafe fn read_at(
        &mut self,
        ud: u64,
        file: u16,
        buf: *mut u8,
        len: u32,
        offset: u64,
    ) -> Result<(), coagomem_core::io::SqFull> {
        // SAFETY: the caller supplies the same live buffer required by this trait.
        unsafe { self.0.read_at(ud, file, buf, len, offset)? };
        panic!("simulated unwind after read submission");
    }
}

#[derive(Default)]
pub struct RejectSubmit {
    pub calls: usize,
}
impl coagomem_core::io::IoBackend for RejectSubmit {
    unsafe fn write_at(
        &mut self,
        _ud: u64,
        _file: u16,
        _buf: *const u8,
        _len: u32,
        _offset: u64,
        _dsync: bool,
    ) -> Result<(), coagomem_core::io::SqFull> {
        self.calls += 1;
        Err(coagomem_core::io::SqFull)
    }
    unsafe fn read_at(
        &mut self,
        _ud: u64,
        _file: u16,
        _buf: *mut u8,
        _len: u32,
        _offset: u64,
    ) -> Result<(), coagomem_core::io::SqFull> {
        self.calls += 1;
        Err(coagomem_core::io::SqFull)
    }
}
