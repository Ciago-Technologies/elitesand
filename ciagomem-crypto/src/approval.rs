//! Public-key attestations. Verifiers contain no signing material.
use crate::{content_digest, ct_eq};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
pub struct AttestationSigner(SigningKey);
#[derive(Debug, Clone)]
pub struct AttestationVerifier(VerifyingKey);
impl AttestationSigner {
    /// Trusted issuer only. Store this outside agent processes; seed comes from a KMS/CSPRNG.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }
    pub fn verifier(&self) -> AttestationVerifier {
        AttestationVerifier(self.0.verifying_key())
    }
    pub fn provenance(&self, claims: Provenance) -> SignedProvenance {
        let signature = self.0.sign(&claims.message()).to_bytes();
        SignedProvenance { claims, signature }
    }
    pub fn endorse(&self, approval: Approval) -> Endorsement {
        let signature = self.0.sign(&approval.message()).to_bytes();
        Endorsement {
            approval,
            signature,
        }
    }
}
impl AttestationVerifier {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, &'static str> {
        Ok(Self(
            VerifyingKey::from_bytes(&bytes).map_err(|_| "invalid public key")?,
        ))
    }
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
    fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        self.0
            .verify_strict(msg, &Signature::from_bytes(sig))
            .is_ok()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    pub tenant: u128,
    pub session: u128,
    pub integrity: u8,
    pub sources: u64,
    pub readers: Vec<u128>,
    pub digest: [u8; 32],
    pub expires_at: u64,
}
impl Provenance {
    fn message(&self) -> Vec<u8> {
        let mut out = b"ciagomem/provenance/v2".to_vec();
        out.extend(self.tenant.to_le_bytes());
        out.extend(self.session.to_le_bytes());
        out.push(self.integrity);
        out.extend(self.sources.to_le_bytes());
        out.extend(self.expires_at.to_le_bytes());
        out.extend(self.digest);
        out.extend((self.readers.len() as u64).to_le_bytes());
        for r in &self.readers {
            out.extend(r.to_le_bytes());
        }
        out
    }
}
#[derive(Debug, Clone)]
pub struct SignedProvenance {
    pub claims: Provenance,
    pub signature: [u8; 64],
}
impl SignedProvenance {
    pub fn verify(
        &self,
        key: &AttestationVerifier,
        tenant: u128,
        session: u128,
        content: &[u8],
        now: u64,
    ) -> bool {
        self.claims.tenant == tenant
            && self.claims.session == session
            && self.claims.sources != 0
            && now < self.claims.expires_at
            && self.claims.readers.len() <= 4
            && self.claims.integrity <= 3
            && ct_eq(&self.claims.digest, &content_digest(content))
            && key.verify(&self.claims.message(), &self.signature)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    pub tenant: u128,
    pub session: u128,
    pub nonce: u128,
    pub field: u8,
    pub sources: u64,
    pub digest: [u8; 32],
    pub sink: u128,
    pub tool: String,
    pub expires_at: u64,
}
impl Approval {
    fn message(&self) -> Vec<u8> {
        let mut out = b"ciagomem/approval/v2".to_vec();
        out.extend(self.tenant.to_le_bytes());
        out.extend(self.session.to_le_bytes());
        out.extend(self.nonce.to_le_bytes());
        out.push(self.field);
        out.extend(self.sources.to_le_bytes());
        out.extend(self.digest);
        out.extend(self.sink.to_le_bytes());
        out.extend(self.expires_at.to_le_bytes());
        out.extend((self.tool.len() as u64).to_le_bytes());
        out.extend(self.tool.as_bytes());
        out
    }
}
#[derive(Debug, Clone)]
pub struct Endorsement {
    pub approval: Approval,
    pub signature: [u8; 64],
}
impl Endorsement {
    pub fn verify(&self, key: &AttestationVerifier, expected: &Approval, now: u64) -> bool {
        expected.sources != 0
            && expected.nonce != 0
            && expected.tool.len() <= 256
            && now < expected.expires_at
            && self.approval == *expected
            && key.verify(&self.approval.message(), &self.signature)
    }
}
