//! Identity metadata journal with an independent compare-and-set rollback anchor.
//! The anchor must survive restoration/cloning of this journal; a second file in the
//! same backup is not such an anchor. No insecure unanchored open path is provided.
use ciagomem_crypto::{Prf, content_digest};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentityHead {
    pub sequence: u64,
    pub hash: [u8; 32],
}
impl Default for IdentityHead {
    fn default() -> Self {
        Self {
            sequence: 0,
            hash: [0; 32],
        }
    }
}
pub trait IdentityAnchor: Send + Sync {
    fn initialize(&self, uuid: [u8; 16]) -> std::io::Result<()>;
    fn head(&self, uuid: [u8; 16]) -> std::io::Result<IdentityHead>;
    fn advance(
        &self,
        uuid: [u8; 16],
        expected: IdentityHead,
        next: IdentityHead,
    ) -> std::io::Result<()>;
}
#[derive(Debug, Clone)]
struct Entry {
    namespace: String,
    name: String,
    incarnation: u128,
}
pub struct IncarnationStore {
    file: File,
    uuid: [u8; 16],
    head: IdentityHead,
    anchor: Arc<dyn IdentityAnchor>,
    entries: HashMap<u64, Entry>,
    failed: bool,
    pub recovered_tail_bytes: u64,
}
fn failure(s: &str) -> std::io::Error {
    std::io::Error::other(s)
}
fn sync_parent(path: &Path) -> std::io::Result<()> {
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()
}
impl IncarnationStore {
    pub fn create(path: &Path, anchor: Arc<dyn IdentityAnchor>) -> std::io::Result<Self> {
        let mut uuid = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut uuid)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(path)?;
        file.try_lock().map_err(std::io::Error::other)?;
        file.write_all(b"CIAIDR6\0")?;
        file.write_all(&uuid)?;
        file.sync_all()?;
        sync_parent(path)?;
        anchor.initialize(uuid)?;
        Ok(Self {
            file,
            uuid,
            head: IdentityHead::default(),
            anchor,
            entries: HashMap::new(),
            failed: false,
            recovered_tail_bytes: 0,
        })
    }
    pub fn open(path: &Path, anchor: Arc<dyn IdentityAnchor>) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        file.try_lock().map_err(std::io::Error::other)?;
        let len = file.metadata()?.len();
        let mut header = [0u8; 24];
        file.read_exact(&mut header)?;
        if &header[..8] != b"CIAIDR6\0" {
            return Err(failure("unsupported identity journal format"));
        }
        let uuid = header[8..].try_into().unwrap();
        let anchored = anchor.head(uuid)?;
        let mut head = IdentityHead::default();
        let mut entries = HashMap::new();
        let mut offset = 24u64;
        while head.sequence < anchored.sequence {
            let mut size = [0; 4];
            file.read_exact(&mut size)
                .map_err(|_| failure("RolledBack: acknowledged identity record missing"))?;
            let size = u32::from_le_bytes(size) as usize;
            if !(73..=16_384).contains(&size) {
                return Err(failure("corrupt acknowledged identity record"));
            }
            let mut record = vec![0; size];
            file.read_exact(&mut record)
                .map_err(|_| failure("RolledBack: truncated identity prefix"))?;
            let (payload, hash) = record.split_at(size - 32);
            let expected = content_digest(
                &[
                    b"ciagomem/identity/v2".as_slice(),
                    &uuid,
                    &head.hash,
                    payload,
                ]
                .concat(),
            );
            if hash != expected {
                return Err(failure("corrupt identity hash chain"));
            }
            let sequence = u64::from_le_bytes(payload[..8].try_into().unwrap());
            if sequence != head.sequence + 1 {
                return Err(failure("identity sequence mismatch"));
            }
            let op = payload[8];
            let id = u64::from_le_bytes(payload[9..17].try_into().unwrap());
            let inc = u128::from_le_bytes(payload[17..33].try_into().unwrap());
            let ns_len = u32::from_le_bytes(payload[33..37].try_into().unwrap()) as usize;
            let name_len = u32::from_le_bytes(payload[37..41].try_into().unwrap()) as usize;
            if 41 + ns_len + name_len != payload.len() {
                return Err(failure("invalid identity strings"));
            }
            let namespace = String::from_utf8(payload[41..41 + ns_len].to_vec())
                .map_err(|_| failure("invalid UTF8"))?;
            let name = String::from_utf8(payload[41 + ns_len..].to_vec())
                .map_err(|_| failure("invalid UTF8"))?;
            match op {
                1 => {
                    if entries
                        .insert(
                            id,
                            Entry {
                                namespace,
                                name,
                                incarnation: inc,
                            },
                        )
                        .is_some()
                    {
                        return Err(failure("duplicate live identity"));
                    }
                }
                2 => {
                    let old = entries
                        .remove(&id)
                        .ok_or_else(|| failure("missing deleted identity"))?;
                    if old.incarnation != inc {
                        return Err(failure("wrong deletion incarnation"));
                    }
                }
                _ => return Err(failure("invalid identity operation")),
            }
            head = IdentityHead {
                sequence,
                hash: expected,
            };
            offset += 4 + size as u64;
        }
        if head != anchored {
            return Err(failure(
                "RolledBack: journal head does not match independent anchor",
            ));
        }
        let recovered_tail_bytes = len
            .checked_sub(offset)
            .ok_or_else(|| failure("truncated identity journal"))?;
        // Only bytes beyond the anchored acknowledged prefix may be discarded. Corrupt
        // data inside that prefix always fails closed; it is never called an unacked tail.
        if recovered_tail_bytes > 0 {
            file.set_len(offset)?;
            file.sync_all()?;
            sync_parent(path)?;
        }
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            file,
            uuid,
            head,
            anchor,
            entries,
            failed: false,
            recovered_tail_bytes,
        })
    }
    fn append(
        &mut self,
        op: u8,
        id: u64,
        inc: u128,
        namespace: &str,
        name: &str,
    ) -> std::io::Result<()> {
        if self.failed {
            return Err(failure("identity journal fenced"));
        }
        let seq = self
            .head
            .sequence
            .checked_add(1)
            .ok_or_else(|| failure("identity sequence exhausted"))?;
        let mut payload = Vec::new();
        payload.extend(seq.to_le_bytes());
        payload.push(op);
        payload.extend(id.to_le_bytes());
        payload.extend(inc.to_le_bytes());
        payload.extend((namespace.len() as u32).to_le_bytes());
        payload.extend((name.len() as u32).to_le_bytes());
        payload.extend(namespace.as_bytes());
        payload.extend(name.as_bytes());
        let hash = content_digest(
            &[
                b"ciagomem/identity/v2".as_slice(),
                &self.uuid,
                &self.head.hash,
                &payload,
            ]
            .concat(),
        );
        let mut record = ((payload.len() + 32) as u32).to_le_bytes().to_vec();
        record.extend(payload);
        record.extend(hash);
        let next = IdentityHead {
            sequence: seq,
            hash,
        };
        let write = self
            .file
            .write_all(&record)
            .and_then(|_| self.file.sync_all())
            .and_then(|_| self.anchor.advance(self.uuid, self.head, next));
        if let Err(e) = write {
            self.failed = true;
            return Err(e);
        }
        self.head = next;
        Ok(())
    }
}
pub struct TenantRegistry {
    store: IncarnationStore,
}
#[derive(Debug)]
pub enum RegistryError {
    Collision,
    Exists,
    Missing,
    InvalidName,
    Persistence(std::io::Error),
}
fn identity(k: &impl Prf, ns: &str, name: &str) -> Result<u64, RegistryError> {
    if ns.len() > 4096 || name.is_empty() || name.len() > 4096 {
        return Err(RegistryError::InvalidName);
    }
    let msg = [
        (ns.len() as u64).to_le_bytes().as_slice(),
        ns.as_bytes(),
        name.as_bytes(),
    ]
    .concat();
    Ok(u64::from_le_bytes(
        k.mac128(b"ciagomem/tenant/v1", &msg)[..8]
            .try_into()
            .unwrap(),
    ))
}
impl TenantRegistry {
    pub fn new(store: IncarnationStore) -> Self {
        Self { store }
    }
    pub fn lookup(&self, k: &impl Prf, ns: &str, name: &str) -> Result<(u64, u128), RegistryError> {
        let id = identity(k, ns, name)?;
        let e = self.store.entries.get(&id).ok_or(RegistryError::Missing)?;
        if e.namespace != ns || e.name != name {
            return Err(RegistryError::Collision);
        }
        Ok((id, e.incarnation))
    }
    pub fn create(
        &mut self,
        k: &impl Prf,
        ns: &str,
        name: &str,
    ) -> Result<(u64, u128), RegistryError> {
        let id = identity(k, ns, name)?;
        match self.lookup(k, ns, name) {
            Ok(_) => return Err(RegistryError::Exists),
            Err(RegistryError::Missing) => {}
            Err(e) => return Err(e),
        }
        let inc = ciagomem_crypto::random_id().map_err(RegistryError::Persistence)?;
        self.store
            .append(1, id, inc, ns, name)
            .map_err(RegistryError::Persistence)?;
        self.store.entries.insert(
            id,
            Entry {
                namespace: ns.into(),
                name: name.into(),
                incarnation: inc,
            },
        );
        Ok((id, inc))
    }
    pub fn register(
        &mut self,
        k: &impl Prf,
        ns: &str,
        name: &str,
    ) -> Result<(u64, u128), RegistryError> {
        match self.lookup(k, ns, name) {
            Ok(v) => Ok(v),
            Err(RegistryError::Missing) => self.create(k, ns, name),
            Err(e) => Err(e),
        }
    }
    pub fn delete(&mut self, k: &impl Prf, ns: &str, name: &str) -> Result<(), RegistryError> {
        let (id, inc) = self.lookup(k, ns, name)?;
        self.store
            .append(2, id, inc, ns, name)
            .map_err(RegistryError::Persistence)?;
        self.store.entries.remove(&id);
        Ok(())
    }
}
