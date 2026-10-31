use crate::{Error, Result};
use ciagomem_core::Label;
use std::{cmp::Ordering, collections::BTreeMap, sync::Arc};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ActorRef {
    pub id: u64,
    pub incarnation: u128,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Ref(ActorRef),
}
pub type Fields = BTreeMap<String, Scalar>;
#[derive(Debug, Clone)]
pub struct Record {
    pub actor: ActorRef,
    pub tenant: u128,
    pub schema_version: u32,
    pub fields: Fields,
    pub label: Label,
    pub embedding: Vec<f32>,
}
impl Record {
    pub fn new(actor: ActorRef, tenant: u128, fields: Fields, label: Label) -> Self {
        Self {
            actor,
            tenant,
            schema_version: 1,
            fields,
            label,
            embedding: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Null,
    Bool(bool),
    Int(i64),
    Float(u64),
    Text(String),
    Ref(ActorRef),
}
impl Scalar {
    pub fn key(&self) -> Result<Key> {
        Ok(match self {
            Self::Null => Key::Null,
            Self::Bool(b) => Key::Bool(*b),
            Self::Int(i) => Key::Int(*i),
            Self::Float(f) if f.is_finite() => Key::Float(if *f == 0.0 { 0 } else { f.to_bits() }),
            Self::Float(_) => return Err(Error::InvalidValue),
            Self::Text(s) => Key::Text(s.clone()),
            Self::Ref(r) => Key::Ref(*r),
        })
    }
    pub fn number(&self) -> Result<f64> {
        match self {
            Self::Int(i) if i.unsigned_abs() <= 1 << 53 => Ok(*i as f64),
            Self::Float(f) if f.is_finite() => Ok(*f),
            _ => Err(Error::Type),
        }
    }
    pub fn compare(&self, b: &Self) -> Result<Option<Ordering>> {
        Ok(match (self, b) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(b)),
            (Self::Text(a), Self::Text(b)) => Some(a.cmp(b)),
            (Self::Bool(a), Self::Bool(b)) => Some(a.cmp(b)),
            (Self::Ref(a), Self::Ref(b)) => Some(a.cmp(b)),
            (Self::Null, Self::Null) => Some(Ordering::Equal),
            (Self::Null, _) | (_, Self::Null) => None,
            (Self::Float(a), Self::Float(b)) if a.is_finite() && b.is_finite() => a.partial_cmp(b),
            (Self::Int(a), Self::Float(b)) if b.is_finite() => Some(compare_integer_float(*a, *b)),
            (Self::Float(a), Self::Int(b)) if a.is_finite() => {
                Some(compare_integer_float(*b, *a).reverse())
            }
            _ => None,
        })
    }
    pub fn bytes(&self) -> usize {
        match self {
            Self::Text(s) => 32 + s.len(),
            _ => 32,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub record: Arc<Record>,
    pub image: Arc<[u8]>,
}
#[derive(Debug, Clone)]
pub struct Limits {
    pub rows_per_core: usize,
    pub bytes_per_core: usize,
    pub query_work: usize,
    pub result_bytes: usize,
    pub groups: usize,
    pub join_rows: usize,
    pub fields: usize,
    pub text_bytes: usize,
    pub vector_dims: usize,
    pub cascade_rows: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            rows_per_core: 10_000,
            bytes_per_core: 64 << 20,
            query_work: 100_000,
            result_bytes: 1 << 20,
            groups: 1000,
            join_rows: 1000,
            fields: 128,
            text_bytes: 64 << 10,
            vector_dims: 4096,
            cascade_rows: 1000,
        }
    }
}
impl Limits {
    pub fn validate(&self, r: &Record) -> Result<usize> {
        if r.actor.id == 0 || r.actor.incarnation == 0 || r.tenant == 0 || r.schema_version == 0 {
            return Err(Error::InvalidValue);
        }
        if r.fields.len() > self.fields || r.embedding.len() > self.vector_dims {
            return Err(Error::Budget);
        }
        if r.embedding.iter().any(|x| !x.is_finite() || x.abs() > 1e10) {
            return Err(Error::InvalidValue);
        }
        let mut bytes = 512usize + r.embedding.len() * 8;
        for (name, value) in &r.fields {
            if name.len() > 256 || name.is_empty() {
                return Err(Error::InvalidValue);
            }
            value.key()?;
            if let Scalar::Ref(a) = value {
                if a.id == 0 || a.incarnation == 0 {
                    return Err(Error::InvalidValue);
                }
            }
            if let Scalar::Text(s) = value {
                if s.len() > self.text_bytes {
                    return Err(Error::Budget);
                };
                bytes = bytes.checked_add(s.len() * 160).ok_or(Error::Budget)?;
            }
            bytes = bytes
                .checked_add(256 + name.len() * 2 + value.bytes() * 2)
                .ok_or(Error::Budget)?;
        }
        Ok(bytes)
    }
}

fn compare_integer_float(i: i64, f: f64) -> Ordering {
    if f < i64::MIN as f64 {
        return Ordering::Greater;
    }
    if f >= 9_223_372_036_854_775_808.0 {
        return Ordering::Less;
    }
    let truncated = f as i64;
    match i.cmp(&truncated) {
        Ordering::Equal => {
            if f.fract() > 0.0 {
                Ordering::Less
            } else if f.fract() < 0.0 {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        other => other,
    }
}
