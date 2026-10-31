//! Tenant-scoped labels, authenticated endorsements, reference tokens and identity allocation.
use ciagomem_crypto::{
    Approval, AttestationVerifier, Endorsement, Prf, SignedProvenance, VerifiedMacaroon,
    content_digest, ct_eq,
};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultLoc {
    pub segment: u32,
    pub offset: u64,
    pub len: u32,
    pub generation: u64,
    pub digest: [u8; 32],
}
pub use ciagomem_ql::RefLiteral as RefToken;
#[derive(Debug, PartialEq, Eq)]
pub enum RefDenied {
    Denied,
}
#[derive(Debug, Clone, Copy)]
pub struct RefScope {
    pub tenant: u128,
    pub session: u128,
    pub actor: u64,
    pub expires_at: u64,
}
pub fn mint_ref(k: &impl Prf, scope: &RefScope, id: u64, loc: &VaultLoc) -> RefToken {
    let msg = [
        scope.tenant.to_le_bytes().as_slice(),
        &scope.session.to_le_bytes(),
        &scope.actor.to_le_bytes(),
        &scope.expires_at.to_le_bytes(),
        &id.to_le_bytes(),
        &loc.segment.to_le_bytes(),
        &loc.offset.to_le_bytes(),
        &loc.len.to_le_bytes(),
        &loc.generation.to_le_bytes(),
        &loc.digest,
    ]
    .concat();
    RefToken {
        id,
        mac: k.mac128(b"ciagomem/ref/v2", &msg),
    }
}
pub fn resolve_ref<'a>(
    k: &impl Prf,
    scope: &RefScope,
    index: &'a HashMap<u64, VaultLoc>,
    tok: &RefToken,
    now: u64,
) -> Result<&'a VaultLoc, RefDenied> {
    // A single error class hides existence; missing entries still execute a MAC comparison.
    let dummy = VaultLoc {
        segment: 0,
        offset: 0,
        len: 0,
        generation: 0,
        digest: [0; 32],
    };
    let found = index.get(&tok.id);
    let loc = found.unwrap_or(&dummy);
    let valid = ct_eq(&tok.mac, &mint_ref(k, scope, tok.id, loc).mac);
    if !valid
        || found.is_none()
        || scope.actor == 0
        || scope.session == 0
        || now >= scope.expires_at
    {
        return Err(RefDenied::Denied);
    }
    Ok(found.unwrap())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Integrity {
    External,
    AgentDerived,
    UserDirect,
    Policy,
}
/// Security metadata is opaque. Trusted integrity requires signed, content-bound provenance.
/// ```compile_fail
/// use ciagomem_core::{Label,Integrity};
/// let label=Label::new(Integrity::Policy,1,vec![1]);
/// ```
/// ```compile_fail
/// use ciagomem_core::{Label,Integrity};
/// let mut label=Label::external(1,vec![1]).unwrap();
/// label.integrity=Integrity::Policy;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    integrity: Integrity,
    sources: u64,
    readers: Vec<u128>,
    digest: Option<[u8; 32]>,
    issuer: Option<[u8; 32]>,
    scope: Option<(u128, u128, u128)>,
    expires_at: u64,
}
impl Label {
    pub fn external(sources: u64, mut readers: Vec<u128>) -> Result<Self, &'static str> {
        readers.sort_unstable();
        readers.dedup();
        if readers.len() > 4 {
            return Err("too many readers");
        }
        Ok(Self {
            integrity: Integrity::External,
            sources,
            readers,
            digest: None,
            issuer: None,
            scope: None,
            expires_at: u64::MAX,
        })
    }
    pub fn attested(
        proof: &SignedProvenance,
        key: &AttestationVerifier,
        tenant: u128,
        session: u128,
        boot_epoch: u128,
        content: &[u8],
        now: u64,
    ) -> Result<Self, &'static str> {
        if !proof.verify(key, tenant, session, boot_epoch, content, now) {
            return Err("invalid provenance");
        }
        let mut l = Self::external(proof.claims.sources, proof.claims.readers.clone())?;
        l.integrity = match proof.claims.integrity {
            0 => Integrity::External,
            1 => Integrity::AgentDerived,
            2 => Integrity::UserDirect,
            3 => Integrity::Policy,
            _ => return Err("unknown integrity"),
        };
        l.digest = Some(content_digest(content));
        l.issuer = Some(key.to_bytes());
        l.scope = Some((tenant, session, boot_epoch));
        l.expires_at = proof.claims.expires_at;
        Ok(l)
    }
    pub fn integrity(&self) -> Integrity {
        self.integrity
    }
    pub fn sources(&self) -> u64 {
        self.sources
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
            digest: None,
            issuer: if self.issuer == b.issuer {
                self.issuer
            } else {
                None
            },
            scope: if self.scope == b.scope {
                self.scope
            } else {
                None
            },
            expires_at: self.expires_at.min(b.expires_at),
        }
    }
    pub fn derived(content: &[u8], parents: &[Label]) -> Self {
        let mut l = label_pllm_output(parents);
        l.digest = Some(content_digest(content));
        l
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
    ReaderDenied,
    InvalidProvenance,
    Replay,
    Capacity,
    WrongAction,
    ExpiredSession,
}
/// Approval times use the monotonic nanosecond clock of the current boot epoch.
pub const MAX_APPROVAL_TTL_NS: u64 = 300_000_000_000;
pub struct NonceTracker {
    seen: HashSet<(u128, u128, u128)>,
    expiry: BinaryHeap<Reverse<(u64, u128, u128, u128)>>,
    counts: HashMap<u128, usize>,
    per_tenant: usize,
    max_tenants: usize,
    boot_epoch: u128,
    /// High-water clock: eviction and expiry never move backwards (R7-3).
    clock: u64,
}
impl NonceTracker {
    pub fn new(per_tenant: usize, max_tenants: usize, boot_epoch: u128) -> Self {
        assert!(per_tenant > 0 && max_tenants > 0 && boot_epoch != 0);
        Self {
            seen: HashSet::new(),
            expiry: BinaryHeap::new(),
            counts: HashMap::new(),
            per_tenant,
            max_tenants,
            boot_epoch,
            clock: 0,
        }
    }
    fn observe(&mut self, now: u64) -> u64 {
        self.clock = self.clock.max(now);
        self.clock
    }
    fn consume(
        &mut self,
        tenant: u128,
        session: u128,
        nonce: u128,
        expires: u64,
        now: u64,
    ) -> Result<(), FlowDenied> {
        self.clock = self.clock.max(now);
        let now = self.clock;
        if expires <= now || expires - now > MAX_APPROVAL_TTL_NS {
            return Err(FlowDenied::InvalidEndorsement);
        }
        while let Some(&Reverse((expires, tenant, session, nonce))) = self.expiry.peek() {
            if expires > now {
                break;
            }
            self.expiry.pop();
            self.seen.remove(&(tenant, session, nonce));
            let count = self.counts.get_mut(&tenant).unwrap();
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&tenant);
            }
        }
        let key = (tenant, session, nonce);
        if self.seen.contains(&key) {
            return Err(FlowDenied::Replay);
        }
        let count = self.counts.get(&tenant).copied().unwrap_or(0);
        if count >= self.per_tenant || count == 0 && self.counts.len() >= self.max_tenants {
            return Err(FlowDenied::Capacity);
        }
        self.seen.insert(key);
        self.counts.insert(tenant, count + 1);
        self.expiry.push(Reverse((expires, tenant, session, nonce)));
        Ok(())
    }
}
pub struct AdmissionContext<'a> {
    pub session: &'a VerifiedMacaroon,
    pub reader: u128,
    pub content: &'a [u8],
    pub now: u64,
    pub nonce: u128,
    pub tool: &'a str,
    pub sink: u128,
    pub verifier: &'a AttestationVerifier,
}
#[derive(Debug)]
pub struct FrameValue {
    label: Label,
    tenant: u128,
    session: u128,
    boot_epoch: u128,
    digest: [u8; 32],
    tool: String,
    sink: u128,
    expires_at: u64,
    action: bool,
    used: bool,
}
impl FrameValue {
    pub fn label(&self) -> &Label {
        &self.label
    }
}
pub fn admit_to_frame(
    field: FrameField,
    mut label: Label,
    endorsement: Option<&Endorsement>,
    ctx: &AdmissionContext<'_>,
    nonces: &mut NonceTracker,
) -> Result<FrameValue, FlowDenied> {
    let now = nonces.observe(ctx.now);
    if !label.readers.contains(&ctx.reader) {
        return Err(FlowDenied::ReaderDenied);
    }
    if now >= ctx.session.expires_at() {
        return Err(FlowDenied::ExpiredSession);
    }
    if ctx.session.boot_epoch() != nonces.boot_epoch {
        return Err(FlowDenied::Replay);
    }
    let tenant = ctx.session.tenant_incarnation();
    let session = ctx.session.session_id();
    let digest = content_digest(ctx.content);
    if label.integrity != Integrity::External
        && (label.issuer != Some(ctx.verifier.to_bytes())
            || label.scope != Some((tenant, session, ctx.session.boot_epoch()))
            || label.digest != Some(digest)
            || now >= label.expires_at)
    {
        return Err(FlowDenied::InvalidProvenance);
    }
    let mut action = false;
    if let Some(e) = endorsement {
        if label.integrity == Integrity::External {
            return Err(FlowDenied::InvalidEndorsement);
        }
        let expected = Approval {
            tenant,
            session,
            boot_epoch: ctx.session.boot_epoch(),
            nonce: ctx.nonce,
            field: field as u8,
            sources: label.sources,
            digest,
            sink: ctx.sink,
            tool: ctx.tool.to_owned(),
            expires_at: e.approval.expires_at,
        };
        if !e.verify(ctx.verifier, &expected, now) {
            return Err(FlowDenied::InvalidEndorsement);
        }
        if matches!(field, FrameField::ActiveIntent | FrameField::Playbook) {
            label.integrity = Integrity::UserDirect;
        }
        action = true;
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
    if let Some(e) = endorsement {
        nonces.consume(tenant, session, ctx.nonce, e.approval.expires_at, now)?;
    }
    Ok(FrameValue {
        expires_at: label
            .expires_at
            .min(endorsement.map_or(u64::MAX, |e| e.approval.expires_at))
            .min(ctx.session.expires_at()),
        label,
        tenant,
        session,
        boot_epoch: ctx.session.boot_epoch(),
        digest,
        tool: ctx.tool.into(),
        sink: ctx.sink,
        action,
        used: false,
    })
}
/// Labels alone never authorize side effects. Use the one-use action permit below.
pub fn gateway_allow(side_effecting: bool, label: &Label, sink: u128) -> bool {
    !side_effecting && label.readers.contains(&sink)
}
pub fn authorize_tool_call(
    value: &mut FrameValue,
    session: &VerifiedMacaroon,
    tool: &str,
    sink: u128,
    args: &[u8],
    now: u64,
) -> Result<(), FlowDenied> {
    if value.used
        || !value.action
        || value.tenant != session.tenant_incarnation()
        || value.session != session.session_id()
        || value.boot_epoch != session.boot_epoch()
        || value.tool != tool
        || value.sink != sink
        || value.digest != content_digest(args)
        || now >= value.expires_at
        || now >= session.expires_at()
        || !value.label.readers.contains(&sink)
    {
        return Err(FlowDenied::WrongAction);
    }
    value.used = true;
    Ok(())
}
pub fn label_pllm_output(context: &[Label]) -> Label {
    let Some((first, rest)) = context.split_first() else {
        return Label::external(0, vec![]).unwrap();
    };
    let mut out = rest.iter().fold(first.clone(), |a, b| a.join(b));
    out.integrity = out.integrity.min(Integrity::AgentDerived);
    out.digest = None;
    out.issuer = None;
    out.scope = None;
    out
}
pub fn label_qllm_output(context: &[Label]) -> Label {
    let mut out = label_pllm_output(context);
    out.integrity = Integrity::External;
    out
}

pub mod identity;
pub use identity::*;
