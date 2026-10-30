//! Tenant-scoped labels, authenticated endorsements, reference tokens and identity allocation.
use ciagomem_crypto::{Endorsement, Prf, ct_eq};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultLoc {
    pub segment: u32,
    pub offset: u64,
    pub len: u32,
    pub generation: u64,
    pub blake3_256: [u8; 32],
}
#[derive(Debug, Clone)]
pub struct RefToken {
    pub id: u64,
    pub mac: [u8; 16],
}
#[derive(Debug, PartialEq, Eq)]
pub enum RefDenied {
    NotFound,
    BadMac,
}
pub fn mint_ref(k: &impl Prf, tenant: u128, id: u64, loc: &VaultLoc) -> RefToken {
    let msg = [
        tenant.to_le_bytes().as_slice(),
        &id.to_le_bytes(),
        &loc.segment.to_le_bytes(),
        &loc.offset.to_le_bytes(),
        &loc.len.to_le_bytes(),
        &loc.generation.to_le_bytes(),
        &loc.blake3_256,
    ]
    .concat();
    RefToken {
        id,
        mac: k.mac128(b"ciagomem/ref/v1", &msg),
    }
}
pub fn resolve_ref<'a>(
    k: &impl Prf,
    tenant: u128,
    index: &'a HashMap<u64, VaultLoc>,
    tok: &RefToken,
) -> Result<&'a VaultLoc, RefDenied> {
    let loc = index.get(&tok.id).ok_or(RefDenied::NotFound)?;
    if !ct_eq(&tok.mac, &mint_ref(k, tenant, tok.id, loc).mac) {
        return Err(RefDenied::BadMac);
    }
    Ok(loc)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Integrity {
    External,
    AgentDerived,
    UserDirect,
    Policy,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub integrity: Integrity,
    pub sources: u64,
    readers: Vec<u128>,
}
impl Label {
    /// Explicit principal IDs; no modulo operation or global 64-bit tenant bitmap.
    pub fn new(
        integrity: Integrity,
        sources: u64,
        mut readers: Vec<u128>,
    ) -> Result<Self, &'static str> {
        readers.sort_unstable();
        readers.dedup();
        if readers.len() > 4 {
            return Err("reader set exceeds inline policy limit");
        }
        Ok(Self {
            integrity,
            sources,
            readers,
        })
    }
    pub fn readers(&self) -> &[u128] {
        &self.readers
    }
    pub fn join(&self, b: &Self) -> Self {
        Self {
            integrity: self.integrity.min(b.integrity),
            sources: self.sources | b.sources,
            readers: self
                .readers
                .iter()
                .copied()
                .filter(|r| b.readers.binary_search(r).is_ok())
                .collect(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameField {
    HardConstraint,
    ActiveIntent,
    Playbook,
    Fact,
    Var,
}
#[derive(Debug, PartialEq, Eq)]
pub enum FlowDenied {
    InsufficientIntegrity,
    InvalidEndorsement,
}
pub fn admit_to_frame(
    field: FrameField,
    mut label: Label,
    endorsement: Option<&Endorsement>,
    user_key: &impl Prf,
    tenant: u128,
    now: u64,
) -> Result<Label, FlowDenied> {
    if let Some(e) = endorsement {
        if !e.verify(user_key, tenant, label.sources, now)
            || label.integrity != Integrity::AgentDerived
        {
            return Err(FlowDenied::InvalidEndorsement);
        }
        if matches!(field, FrameField::ActiveIntent | FrameField::Playbook) {
            label.integrity = Integrity::UserDirect
        }
    }
    let required = match field {
        FrameField::HardConstraint => Integrity::Policy,
        FrameField::ActiveIntent | FrameField::Playbook => Integrity::UserDirect,
        FrameField::Fact => Integrity::AgentDerived,
        FrameField::Var => Integrity::External,
    };
    if label.integrity < required {
        return Err(FlowDenied::InsufficientIntegrity);
    }
    Ok(label)
}
pub fn gateway_allow(side_effecting: bool, label: &Label, sink: u128) -> bool {
    label.readers.binary_search(&sink).is_ok()
        && (!side_effecting || label.integrity >= Integrity::UserDirect)
}
/// P-LLM outputs inherit every context restriction, and can never acquire human integrity.
pub fn label_pllm_output(context: &[Label]) -> Label {
    let Some((first, rest)) = context.split_first() else {
        return Label::new(Integrity::External, 0, vec![]).unwrap();
    };
    let mut out = rest.iter().fold(first.clone(), |a, b| a.join(b));
    out.integrity = out.integrity.min(Integrity::AgentDerived);
    out
}
pub fn label_qllm_output(context: &[Label]) -> Label {
    let mut l = label_pllm_output(context);
    l.integrity = Integrity::External;
    l
}

/// Durable append-only incarnation reservation journal. One authoritative path per identity domain.
/// The process holds an exclusive file lock. Incomplete/torn records fail closed on attach.
/// This journal must be retained with backups; it is not yet integrated into the WAL superblock.
pub struct IncarnationStore {
    file: File,
    last: u128,
    failed: bool,
}
impl IncarnationStore {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .append(true)
            .open(path)?;
        file.try_lock().map_err(std::io::Error::other)?;
        file.sync_all()?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(Self {
            file,
            last: 0,
            failed: false,
        })
    }
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().read(true).append(true).open(path)?;
        file.try_lock().map_err(std::io::Error::other)?;
        let len = file.metadata()?.len();
        if len % 32 != 0 {
            return Err(std::io::Error::other("incomplete incarnation journal"));
        }
        let mut last = 0u128;
        let mut record = [0u8; 32];
        file.seek(SeekFrom::Start(0))?;
        for _ in 0..len / 32 {
            file.read_exact(&mut record)?;
            let n = u128::from_le_bytes(record[..16].try_into().unwrap());
            let check = u128::from_le_bytes(record[16..].try_into().unwrap());
            if n != last + 1 || check != !n {
                return Err(std::io::Error::other("corrupt incarnation journal"));
            }
            last = n;
        }
        Ok(Self {
            file,
            last,
            failed: false,
        })
    }
    fn reserve(&mut self) -> std::io::Result<u128> {
        if self.failed {
            return Err(std::io::Error::other(
                "incarnation store fenced after I/O error",
            ));
        }
        let n = self
            .last
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("incarnation exhausted"))?;
        let record = [n.to_le_bytes(), (!n).to_le_bytes()].concat();
        if let Err(e) = self
            .file
            .write_all(&record)
            .and_then(|_| self.file.sync_all())
        {
            self.failed = true;
            return Err(e);
        }
        self.last = n;
        Ok(n)
    }
}
pub struct TenantRegistry {
    by_id: HashMap<u64, (String, String, u128)>,
    store: IncarnationStore,
}
#[derive(Debug)]
pub enum RegistryError {
    Collision,
    Persistence(std::io::Error),
}
impl TenantRegistry {
    pub fn new(store: IncarnationStore) -> Self {
        Self {
            by_id: HashMap::new(),
            store,
        }
    }
    pub fn register(
        &mut self,
        k: &impl Prf,
        ns: &str,
        external: &str,
    ) -> Result<(u64, u128), RegistryError> {
        let msg = [
            (ns.len() as u64).to_le_bytes().as_slice(),
            ns.as_bytes(),
            external.as_bytes(),
        ]
        .concat();
        let id = u64::from_le_bytes(
            k.mac128(b"ciagomem/tenant/v1", &msg)[..8]
                .try_into()
                .unwrap(),
        );
        if let Some((n, e, inc)) = self.by_id.get(&id) {
            return if n == ns && e == external {
                Ok((id, *inc))
            } else {
                Err(RegistryError::Collision)
            };
        }
        let counter = self.store.reserve().map_err(RegistryError::Persistence)?;
        let inc = u128::from_le_bytes(k.mac128(
            b"ciagomem/incarnation/v1",
            &[id.to_le_bytes().as_slice(), &counter.to_le_bytes()].concat(),
        ));
        self.by_id.insert(id, (ns.into(), external.into(), inc));
        Ok((id, inc))
    }
}
