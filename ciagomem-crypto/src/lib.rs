//! Standard HMAC-SHA256 provider and authenticated capability primitives.
#![forbid(unsafe_code)]
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
pub fn content_digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
use subtle::ConstantTimeEq;

/// OS entropy is required after restart; failure never falls back to a counter.
pub fn random_id() -> std::io::Result<u128> {
    use std::io::Read;
    let mut random = std::fs::File::open("/dev/urandom")?;
    loop {
        let mut bytes = [0; 16];
        random.read_exact(&mut bytes)?;
        let id = u128::from_le_bytes(bytes);
        if id != 0 {
            return Ok(id);
        }
    }
}
/// Create once per engine start; never restore this value from a data backup.
pub struct SessionEpoch(u128);
impl SessionEpoch {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self(random_id()?))
    }
    pub fn id(&self) -> u128 {
        self.0
    }
}
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
    Audience(u64),
    Session(u128),
    BootEpoch(u128),
    ClientId(u64),
    Unknown(Vec<u8>),
}
fn encode(c: &Caveat) -> Vec<u8> {
    match c {
        Caveat::Tenant(v) => [vec![0], v.to_le_bytes().to_vec()].concat(),
        Caveat::ExpiresAt(v) => [vec![1], v.to_le_bytes().to_vec()].concat(),
        Caveat::PeerUid(v) => [vec![2], v.to_le_bytes().to_vec()].concat(),
        Caveat::Ops(v) => [vec![3], v.to_le_bytes().to_vec()].concat(),
        Caveat::KeyEpoch(v) => [vec![4], v.to_le_bytes().to_vec()].concat(),
        Caveat::Audience(v) => [vec![5], v.to_le_bytes().to_vec()].concat(),
        Caveat::Session(v) => [vec![6], v.to_le_bytes().to_vec()].concat(),
        Caveat::BootEpoch(v) => [vec![7], v.to_le_bytes().to_vec()].concat(),
        Caveat::ClientId(v) => [vec![8], v.to_le_bytes().to_vec()].concat(),
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
    pub session: u128,
    pub audience: u64,
    pub boot_epoch: u128,
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
    session: u128,
    audience: u64,
    boot_epoch: u128,
    expires_at: u64,
    client_id: Option<u64>,
}
impl VerifiedMacaroon {
    pub fn client_id(&self) -> Option<u64> {
        self.client_id
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }
    pub fn tenant_incarnation(&self) -> u128 {
        self.tenant
    }
    pub fn session_id(&self) -> u128 {
        self.session
    }
    pub fn boot_epoch(&self) -> u128 {
        self.boot_epoch
    }
    pub fn audience(&self) -> u64 {
        self.audience
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
        let mut expires_at = u64::MAX;
        let mut client_id = None;
        let (mut session, mut audience, mut boot) = (false, false, false);
        for c in &self.caveats {
            let ok = match c {
                Caveat::Tenant(v) => {
                    t = true;
                    *v == ctx.tenant_incarnation
                }
                Caveat::ExpiresAt(v) => {
                    e = true;
                    expires_at = expires_at.min(*v);
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
                Caveat::Session(v) => {
                    session = true;
                    *v != 0 && *v == ctx.session
                }
                Caveat::Audience(v) => {
                    audience = true;
                    *v == ctx.audience
                }
                Caveat::BootEpoch(v) => {
                    boot = true;
                    *v != 0 && *v == ctx.boot_epoch
                }
                Caveat::ClientId(v) => {
                    let valid = *v != 0 && client_id.is_none_or(|old| old == *v);
                    client_id = Some(*v);
                    valid
                }
                Caveat::Unknown(_) => false,
            };
            if !ok {
                return Err(CaveatError::Denied);
            }
        }
        if !(t && e && p && o && epoch && session && audience && boot) {
            return Err(CaveatError::MissingMandatory);
        }
        Ok(VerifiedMacaroon {
            tenant: ctx.tenant_incarnation,
            ops,
            session: ctx.session,
            audience: ctx.audience,
            boot_epoch: ctx.boot_epoch,
            expires_at,
            client_id,
        })
    }
}
pub mod approval;
pub use approval::*;
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

impl Macaroon {
    /// Bounded capability wire codec. Signature verification remains mandatory after decoding.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CaveatError> {
        if self.caveats.len() > 32 {
            return Err(CaveatError::TooMany);
        }
        let mut out = vec![1, self.caveats.len() as u8];
        for caveat in &self.caveats {
            let bytes = encode(caveat);
            if bytes.len() > 16300 {
                return Err(CaveatError::TooMany);
            }
            out.push(bytes[0]);
            out.extend(((bytes.len() - 1) as u16).to_le_bytes());
            out.extend(&bytes[1..]);
        }
        out.extend(self.signature);
        if out.len() > 16384 {
            return Err(CaveatError::TooMany);
        }
        Ok(out)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CaveatError> {
        if bytes.len() < 34 || bytes.len() > 16384 || bytes[0] != 1 || bytes[1] > 32 {
            return Err(CaveatError::Denied);
        }
        let end = bytes.len() - 32;
        let mut pos = 2;
        let mut caveats = Vec::new();
        for _ in 0..bytes[1] {
            if pos + 3 > end {
                return Err(CaveatError::Denied);
            }
            let tag = bytes[pos];
            let len = u16::from_le_bytes(bytes[pos + 1..pos + 3].try_into().unwrap()) as usize;
            pos += 3;
            if len > end - pos {
                return Err(CaveatError::Denied);
            }
            let v = &bytes[pos..pos + len];
            pos += len;
            let c = match (tag, len) {
                (0, 16) => Caveat::Tenant(u128::from_le_bytes(v.try_into().unwrap())),
                (1, 8) => Caveat::ExpiresAt(u64::from_le_bytes(v.try_into().unwrap())),
                (2, 4) => Caveat::PeerUid(u32::from_le_bytes(v.try_into().unwrap())),
                (3, 8) => Caveat::Ops(u64::from_le_bytes(v.try_into().unwrap())),
                (4, 8) => Caveat::KeyEpoch(u64::from_le_bytes(v.try_into().unwrap())),
                (5, 8) => Caveat::Audience(u64::from_le_bytes(v.try_into().unwrap())),
                (6, 16) => Caveat::Session(u128::from_le_bytes(v.try_into().unwrap())),
                (7, 16) => Caveat::BootEpoch(u128::from_le_bytes(v.try_into().unwrap())),
                (8, 8) => Caveat::ClientId(u64::from_le_bytes(v.try_into().unwrap())),
                _ => return Err(CaveatError::Denied),
            };
            caveats.push(c);
        }
        if pos != end {
            return Err(CaveatError::Denied);
        }
        Ok(Self {
            caveats,
            signature: bytes[end..].try_into().unwrap(),
        })
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    #[test]
    fn capability_codec_preserves_signature_and_rejects_malformed_tlvs() {
        let token = Macaroon::issue(
            &KeyedPrf([3; 32]),
            vec![Caveat::ClientId(42), Caveat::Tenant(11)],
        )
        .unwrap();
        let bytes = token.to_bytes().unwrap();
        let decoded = Macaroon::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.caveats, token.caveats);
        assert_eq!(decoded.signature, token.signature);
        for end in 0..bytes.len() {
            assert!(Macaroon::from_bytes(&bytes[..end]).is_err());
        }
        let mut unknown = bytes.clone();
        unknown[2] = 254;
        assert!(Macaroon::from_bytes(&unknown).is_err());
        let mut surplus = bytes.clone();
        surplus.push(0);
        assert!(Macaroon::from_bytes(&surplus).is_err());
    }
    #[test]
    fn client_binding_cannot_be_attenuated_to_another_client() {
        let key = KeyedPrf([3; 32]);
        let mut token = Macaroon::issue(
            &key,
            vec![
                Caveat::Tenant(1),
                Caveat::ExpiresAt(100),
                Caveat::PeerUid(2),
                Caveat::Ops(1),
                Caveat::KeyEpoch(1),
                Caveat::Audience(1),
                Caveat::Session(1),
                Caveat::BootEpoch(1),
                Caveat::ClientId(42),
            ],
        )
        .unwrap();
        let ctx = RequestCtx {
            tenant_incarnation: 1,
            key_epoch: 1,
            now_ns: 1,
            op_bit: 1,
            peer_uid: 2,
            session: 1,
            audience: 1,
            boot_epoch: 1,
        };
        assert_eq!(token.verify(&key, &ctx).unwrap().client_id(), Some(42));
        token.attenuate(Caveat::ClientId(43)).unwrap();
        assert!(matches!(token.verify(&key, &ctx), Err(CaveatError::Denied)));
    }
}
