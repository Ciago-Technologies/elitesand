//! Core-local bitmap, inverted text and SQ8 indexes. No global index mutex.
use crate::{Error, Key, Record, Result, Scalar};
use coagomem_ql::{BoolExpr, CompOp, Literal, Root, Value};
use roaring::RoaringBitmap;
use std::collections::BTreeMap;
use wide::{CmpGt, i32x8, i64x4};
#[derive(Debug, Clone)]
pub struct Sq8 {
    codes: Vec<i8>,
    scale: f32,
    norm: f32,
}
impl Sq8 {
    pub fn quantize(v: &[f32]) -> Result<Self> {
        if v.is_empty() || v.len() > 4096 || v.iter().any(|x| !x.is_finite() || x.abs() > 1e10) {
            return Err(Error::InvalidValue);
        }
        let peak = v.iter().fold(0f32, |a, x| a.max(x.abs()));
        let scale = if peak == 0.0 {
            1.0
        } else {
            (peak / 127.0).max(f32::MIN_POSITIVE)
        };
        let codes: Vec<i8> = v
            .iter()
            .map(|x| (*x / scale).round().clamp(-127.0, 127.0) as i8)
            .collect();
        let norm = codes
            .iter()
            .map(|x| (*x as f32).powi(2))
            .sum::<f32>()
            .sqrt();
        Ok(Self { codes, scale, norm })
    }
    pub fn cosine(&self, b: &Self) -> Result<f32> {
        if self.codes.len() != b.codes.len() {
            return Err(Error::Type);
        }
        let mut sum = i32x8::splat(0);
        let chunks = self.codes.len() / 8;
        for i in 0..chunks {
            let a = i32x8::new(std::array::from_fn(|j| self.codes[i * 8 + j] as i32));
            let b = i32x8::new(std::array::from_fn(|j| b.codes[i * 8 + j] as i32));
            sum += a * b;
        }
        let tail: i32 = self.codes[chunks * 8..]
            .iter()
            .zip(&b.codes[chunks * 8..])
            .map(|(a, b)| *a as i32 * *b as i32)
            .sum();
        let dot = sum.reduce_add() + tail;
        if self.norm == 0.0 || b.norm == 0.0 {
            return Ok(0.0);
        }
        Ok((dot as f32 / (self.norm * b.norm)).clamp(-1.0, 1.0))
    }
    pub fn scale(&self) -> f32 {
        self.scale
    }
}
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
#[derive(Default)]
pub struct Index {
    pub all: RoaringBitmap,
    eq: BTreeMap<(String, Key), RoaringBitmap>,
    numbers: BTreeMap<String, BTreeMap<u32, i64>>,
    zones: BTreeMap<(String, u32), (i64, i64)>,
    postings: BTreeMap<(String, String), BTreeMap<u32, u32>>,
    lengths: BTreeMap<String, BTreeMap<u32, u32>>,
    pub vectors: BTreeMap<u32, Sq8>,
}
impl Index {
    pub fn insert(&mut self, slot: u32, r: &Record) {
        self.all.insert(slot);
        for (name, value) in &r.fields {
            self.eq
                .entry((name.clone(), value.key().expect("validated value")))
                .or_default()
                .insert(slot);
            if let Scalar::Int(v) = value {
                self.numbers
                    .entry(name.clone())
                    .or_default()
                    .insert(slot, *v);
                self.refresh_zone(name, slot);
            }
            if let Scalar::Text(text) = value {
                let words = tokens(text);
                self.lengths
                    .entry(name.clone())
                    .or_default()
                    .insert(slot, words.len() as u32);
                let mut counts = BTreeMap::new();
                for word in words {
                    *counts.entry(word).or_insert(0u32) += 1;
                }
                for (word, tf) in counts {
                    self.postings
                        .entry((name.clone(), word))
                        .or_default()
                        .insert(slot, tf);
                }
            }
        }
        if !r.embedding.is_empty() {
            self.vectors
                .insert(slot, Sq8::quantize(&r.embedding).expect("validated vector"));
        }
    }
    pub fn remove(&mut self, slot: u32, r: &Record) {
        self.all.remove(slot);
        for (name, value) in &r.fields {
            let key = (name.clone(), value.key().expect("validated value"));
            if let Some(b) = self.eq.get_mut(&key) {
                b.remove(slot);
                if b.is_empty() {
                    self.eq.remove(&key);
                }
            }
            if let Some(n) = self.numbers.get_mut(name) {
                n.remove(&slot);
                if n.is_empty() {
                    self.numbers.remove(name);
                }
            }
            if matches!(value, Scalar::Int(_)) {
                self.refresh_zone(name, slot);
            }
            if let Scalar::Text(text) = value {
                if let Some(d) = self.lengths.get_mut(name) {
                    d.remove(&slot);
                    if d.is_empty() {
                        self.lengths.remove(name);
                    }
                }
                for word in tokens(text) {
                    let key = (name.clone(), word);
                    if let Some(p) = self.postings.get_mut(&key) {
                        p.remove(&slot);
                        if p.is_empty() {
                            self.postings.remove(&key);
                        }
                    }
                }
            }
        }
        self.vectors.remove(&slot);
    }
    fn refresh_zone(&mut self, field: &str, slot: u32) {
        let zone = slot / 64;
        let start = zone * 64;
        let end = start.saturating_add(63);
        let extrema = self.numbers.get(field).and_then(|c| {
            c.range(start..=end)
                .map(|(_, v)| (*v, *v))
                .reduce(|(lo, hi), (v, _)| (lo.min(v), hi.max(v)))
        });
        let key = (field.to_owned(), zone);
        if let Some(bounds) = extrema {
            self.zones.insert(key, bounds);
        } else {
            self.zones.remove(&key);
        }
    }
    pub fn candidate_work(&self, e: &BoolExpr<'_>) -> usize {
        match e {
            BoolExpr::And(es) | BoolExpr::Or(es) => es
                .iter()
                .fold(0usize, |n, e| n.saturating_add(self.candidate_work(e))),
            BoolExpr::Pred(p) => {
                if matches!(p.op, CompOp::Gt | CompOp::Ge | CompOp::Lt | CompOp::Le) {
                    self.all.len() as usize + self.eq.len()
                } else {
                    1
                }
            }
        }
    }
    pub fn candidates(&self, e: &BoolExpr<'_>) -> Option<RoaringBitmap> {
        match e {
            BoolExpr::And(es) => es
                .iter()
                .filter_map(|e| self.candidates(e))
                .reduce(|a, b| a & b),
            BoolExpr::Or(es) => {
                let mut out = RoaringBitmap::new();
                for e in es {
                    out |= self.candidates(e)?;
                }
                Some(out)
            }
            BoolExpr::Pred(p) => {
                if p.field.root != Root::State || p.field.actor.is_some() || p.field.alias.is_some()
                {
                    return None;
                }
                let name = p.field.path.join(".");
                match (&p.op, &p.rhs) {
                    (CompOp::Eq, Value::Lit(lit)) => {
                        let v = literal(*lit).ok()?;
                        if matches!(v, Scalar::Null | Scalar::Float(_)) {
                            return None;
                        }
                        let mut out = self
                            .eq
                            .get(&(name.clone(), v.key().ok()?))
                            .cloned()
                            .unwrap_or_default();
                        if let Scalar::Int(i) = v {
                            if Scalar::Int(i)
                                .compare(&Scalar::Float(i as f64))
                                .ok()
                                .flatten()
                                == Some(std::cmp::Ordering::Equal)
                            {
                                out |= self
                                    .eq
                                    .get(&(name, Scalar::Float(i as f64).key().ok()?))
                                    .cloned()
                                    .unwrap_or_default();
                            }
                        }
                        Some(out)
                    }
                    (op, Value::Lit(Literal::Int(bound)))
                        if matches!(op, CompOp::Gt | CompOp::Ge | CompOp::Lt | CompOp::Le) =>
                    {
                        // Float-valued fields require scalar residual evaluation as well.
                        if self
                            .eq
                            .keys()
                            .any(|(n, k)| n == &name && matches!(k, Key::Float(_)))
                        {
                            return None;
                        }
                        let mut out = RoaringBitmap::new();
                        let Some(column) = self.numbers.get(&name) else {
                            return None;
                        };
                        let mut batch = [(0u32, 0i64); 4];
                        let mut used = 0;
                        let flush = |batch: &[(u32, i64); 4], n: usize, out: &mut RoaringBitmap| {
                            let v = i64x4::new(std::array::from_fn(|j| batch[j].1));
                            let b = i64x4::splat(*bound);
                            let masks = match op {
                                CompOp::Gt => v.cmp_gt(b),
                                CompOp::Lt => b.cmp_gt(v),
                                CompOp::Ge => !b.cmp_gt(v),
                                CompOp::Le => !v.cmp_gt(b),
                                _ => unreachable!(),
                            }
                            .to_array();
                            for j in 0..n {
                                if masks[j] != 0 {
                                    out.insert(batch[j].0);
                                }
                            }
                        };
                        for ((_, zone), (min, max)) in self
                            .zones
                            .range((name.clone(), 0)..=(name.clone(), u32::MAX))
                        {
                            let possible = match op {
                                CompOp::Gt => max > bound,
                                CompOp::Ge => max >= bound,
                                CompOp::Lt => min < bound,
                                CompOp::Le => min <= bound,
                                _ => true,
                            };
                            if !possible {
                                continue;
                            }
                            let start = *zone * 64;
                            let end = start.saturating_add(63);
                            for (&slot, &value) in column.range(start..=end) {
                                batch[used] = (slot, value);
                                used += 1;
                                if used == 4 {
                                    flush(&batch, used, &mut out);
                                    used = 0;
                                }
                            }
                        }
                        if used > 0 {
                            flush(&batch, used, &mut out)
                        }
                        Some(out)
                    }
                    _ => None,
                }
            }
        }
    }
    pub fn bm25(&self, field: &str, query: &str, allowed: &RoaringBitmap) -> BTreeMap<u32, f32> {
        let Some(lengths) = self.lengths.get(field) else {
            return BTreeMap::new();
        };
        let docs: Vec<_> = allowed
            .iter()
            .filter_map(|s| lengths.get(&s).map(|n| (s, *n)))
            .collect();
        let n = docs.len() as f32;
        if n == 0.0 {
            return BTreeMap::new();
        }
        let avg = (docs.iter().map(|(_, n)| *n as u64).sum::<u64>() as f32 / n).max(1.0);
        let mut out = BTreeMap::new();
        let mut terms = tokens(query);
        terms.sort();
        terms.dedup();
        for term in terms {
            let Some(posting) = self.postings.get(&(field.to_owned(), term)) else {
                continue;
            };
            let df = allowed.iter().filter(|s| posting.contains_key(s)).count() as f32;
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            for slot in allowed.iter() {
                if let Some(tf) = posting.get(&slot) {
                    let tf = *tf as f32;
                    let len = lengths[&slot] as f32;
                    *out.entry(slot).or_insert(0.0) +=
                        idf * (tf * 2.2) / (tf + 1.2 * (0.25 + 0.75 * len / avg));
                }
            }
        }
        out
    }
}
pub fn literal(l: Literal<'_>) -> Result<Scalar> {
    Ok(match l {
        Literal::Int(i) => Scalar::Int(i),
        Literal::UInt(u) => Scalar::Int(u.try_into().map_err(|_| Error::Overflow)?),
        Literal::Float(f) if f.is_finite() => Scalar::Float(f),
        Literal::Float(_) => return Err(Error::InvalidValue),
        Literal::Str(s) => Scalar::Text(
            coagomem_ql::unescape(s)
                .map_err(|_| Error::Parse)?
                .into_owned(),
        ),
        Literal::Bool(b) => Scalar::Bool(b),
        Literal::Null => Scalar::Null,
    })
}
