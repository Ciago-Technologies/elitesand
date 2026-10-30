//! Standard HMAC-SHA256 provider and authenticated capability primitives.
#![forbid(unsafe_code)]
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub trait Prf {
    fn mac256(&self, domain: &[u8], msg: &[u8]) -> [u8; 32];
    fn mac128(&self, domain: &[u8], msg: &[u8]) -> [u8; 16] {
        self.mac256(domain, msg)[..16].try_into().unwrap()
    }
}
/// Key material is supplied by the trusted control plane, never by a query.
pub struct KeyedPrf(pub [u8; 32]);
impl Prf for KeyedPrf {
    fn mac256(&self, domain: &[u8], msg: &[u8]) -> [u8; 32] {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).unwrap();
        mac.update(&(domain.len() as u64).to_le_bytes());
        mac.update(domain);
        mac.update(msg);
        mac.finalize().into_bytes().into()
    }
}
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caveat {
    Tenant(u128),
    ExpiresAt(u64),
    PeerUid(u32),
    Ops(u64),
    KeyEpoch(u64),
    Unknown(Vec<u8>),
}
fn encode(c: &Caveat) -> Vec<u8> {
    match c {
        Caveat::Tenant(v) => [vec![0], v.to_le_bytes().to_vec()].concat(),
        Caveat::ExpiresAt(v) => [vec![1], v.to_le_bytes().to_vec()].concat(),
        Caveat::PeerUid(v) => [vec![2], v.to_le_bytes().to_vec()].concat(),
        Caveat::Ops(v) => [vec![3], v.to_le_bytes().to_vec()].concat(),
        Caveat::KeyEpoch(v) => [vec![4], v.to_le_bytes().to_vec()].concat(),
        Caveat::Unknown(v) => [vec![255], v.clone()].concat(),
    }
}
#[derive(Debug, Clone)]
pub struct Macaroon {
    pub caveats: Vec<Caveat>,
    pub signature: [u8; 32],
}
#[derive(Debug, Clone, Copy)]
pub struct RequestCtx {
    pub tenant_incarnation: u128,
    pub key_epoch: u64,
    pub now_ns: u64,
    pub op_bit: u64,
    pub peer_uid: u32,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CaveatError {
    InvalidSignature,
    MissingMandatory,
    Denied,
    TooMany,
}
/// Only successful authentication creates this type; fields cannot be forged externally.
#[derive(Debug)]
pub struct VerifiedMacaroon {
    tenant: u128,
    ops: u64,
}
impl VerifiedMacaroon {
    pub fn tenant_incarnation(&self) -> u128 {
        self.tenant
    }
    pub fn allows(&self, operation: u64) -> bool {
        operation != 0 && self.ops & operation == operation
    }
}
fn signature(k: &impl Prf, cs: &[Caveat]) -> [u8; 32] {
    let mut sig = k.mac256(b"ciagomem/macaroon/v1", b"");
    for c in cs {
        sig = KeyedPrf(sig).mac256(b"ciagomem/caveat/v1", &encode(c));
    }
    sig
}
impl Macaroon {
    /// Trusted issuer operation. Transport and root-key access remain control-plane responsibilities.
    pub fn issue(k: &impl Prf, caveats: Vec<Caveat>) -> Result<Self, CaveatError> {
        if caveats.len() > 32 {
            return Err(CaveatError::TooMany);
        }
        Ok(Self {
            signature: signature(k, &caveats),
            caveats,
        })
    }
    pub fn attenuate(&mut self, c: Caveat) -> Result<(), CaveatError> {
        if self.caveats.len() >= 32 {
            return Err(CaveatError::TooMany);
        }
        self.signature = KeyedPrf(self.signature).mac256(b"ciagomem/caveat/v1", &encode(&c));
        self.caveats.push(c);
        Ok(())
    }
    pub fn verify(&self, k: &impl Prf, ctx: &RequestCtx) -> Result<VerifiedMacaroon, CaveatError> {
        if self.caveats.len() > 32 {
            return Err(CaveatError::TooMany);
        }
        if !ct_eq(&signature(k, &self.caveats), &self.signature) {
            return Err(CaveatError::InvalidSignature);
        }
        let (mut t, mut e, mut p, mut o, mut epoch) = (false, false, false, false, false);
        let mut ops = u64::MAX;
        for c in &self.caveats {
            let ok = match c {
                Caveat::Tenant(v) => {
                    t = true;
                    *v == ctx.tenant_incarnation
                }
                Caveat::ExpiresAt(v) => {
                    e = true;
                    ctx.now_ns < *v
                }
                Caveat::PeerUid(v) => {
                    p = true;
                    *v == ctx.peer_uid
                }
                Caveat::Ops(v) => {
                    o = true;
                    ops &= *v;
                    ctx.op_bit != 0 && *v & ctx.op_bit == ctx.op_bit
                }
                Caveat::KeyEpoch(v) => {
                    epoch = true;
                    *v == ctx.key_epoch
                }
                Caveat::Unknown(_) => false,
            };
            if !ok {
                return Err(CaveatError::Denied);
            }
        }
        if !(t && e && p && o && epoch) {
            return Err(CaveatError::MissingMandatory);
        }
        Ok(VerifiedMacaroon {
            tenant: ctx.tenant_incarnation,
            ops,
        })
    }
}
#[derive(Debug, Clone)]
pub struct Endorsement {
    pub tenant: u128,
    pub sources: u64,
    pub expires_at: u64,
    pub signature: [u8; 32],
}
impl Endorsement {
    /// Caller must hold the authenticated user-channel signing key.
    pub fn issue(k: &impl Prf, tenant: u128, sources: u64, expires_at: u64) -> Self {
        let mut e = Self {
            tenant,
            sources,
            expires_at,
            signature: [0; 32],
        };
        e.signature = k.mac256(b"ciagomem/endorsement/v1", &e.message());
        e
    }
    fn message(&self) -> Vec<u8> {
        [
            self.tenant.to_le_bytes().as_slice(),
            &self.sources.to_le_bytes(),
            &self.expires_at.to_le_bytes(),
        ]
        .concat()
    }
    pub fn verify(&self, k: &impl Prf, tenant: u128, sources: u64, now: u64) -> bool {
        self.tenant == tenant
            && sources & !self.sources == 0
            && now < self.expires_at
            && ct_eq(
                &self.signature,
                &k.mac256(b"ciagomem/endorsement/v1", &self.message()),
            )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rfc4231_hmac_sha256_known_answer() {
        let mut mac = Hmac::<Sha256>::new_from_slice(&[0x0b; 20]).unwrap();
        mac.update(b"Hi There");
        assert_eq!(
            format!("{:x}", mac.finalize().into_bytes()),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn provider_domain_separation() {
        let k = KeyedPrf([1; 32]);
        assert_ne!(k.mac256(b"a", b"bc"), k.mac256(b"ab", b"c"));
    }
}
