//! On-disk formats: CRC32C, log record envelope v2, extent image header.

// ---------------------------------------------------------------------------------
// CRC32C (Castagnoli). SSE4.2 / ARMv8 CRC when available, table fallback otherwise.
// ---------------------------------------------------------------------------------

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static TABLE: [u32; 256] = table();

fn crc_sw(mut c: u32, b: &[u8]) -> u32 {
    for &x in b {
        c = TABLE[((c ^ x as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.2")]
unsafe fn crc_hw(mut c: u32, b: &[u8]) -> u32 {
    use core::arch::x86_64::{_mm_crc32_u64, _mm_crc32_u8};
    let mut chunks = b.chunks_exact(8);
    let mut c64 = c as u64;
    for ch in &mut chunks {
        c64 = _mm_crc32_u64(c64, u64::from_le_bytes(ch.try_into().unwrap()));
    }
    c = c64 as u32;
    for &x in chunks.remainder() {
        c = _mm_crc32_u8(c, x);
    }
    c
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "crc")]
unsafe fn crc_hw(mut c: u32, b: &[u8]) -> u32 {
    use core::arch::aarch64::{__crc32cb, __crc32cd};
    let mut chunks = b.chunks_exact(8);
    for ch in &mut chunks {
        c = __crc32cd(c, u64::from_le_bytes(ch.try_into().unwrap()));
    }
    for &x in chunks.remainder() {
        c = __crc32cb(c, x);
    }
    c
}

#[inline]
fn hw_available() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("sse4.2")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("crc")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// CRC32C over the concatenation of `parts`.
pub fn crc32c(parts: &[&[u8]]) -> u32 {
    let mut c = !0u32;
    let hw = hw_available();
    for p in parts {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            if hw {
                // SAFETY: feature detected at runtime.
                c = unsafe { crc_hw(c, p) };
                continue;
            }
        }
        let _ = hw;
        c = crc_sw(c, p);
    }
    !c
}

// ---------------------------------------------------------------------------------
// Log record envelope v2 (32-byte header, 8-byte aligned records)
// ---------------------------------------------------------------------------------
//
//  off size field
//   0   2   magic  = 0xC0A6
//   2   1   kind   (1 = data, 2 = pad, 3 = tx-decision)
//   3   1   flags
//   4   4   payload_len
//   8   8   lsn       core-local, strictly +1 per record (pad records included)
//  16   4   tenant    dense tenant id
//  20   4   lap       logical_offset / log_capacity (detects records from older laps)
//  24   4   epoch     log incarnation, +1 per open (detects pre-crash leftovers)
//  28   4   crc32c    over bytes 0..28 and the payload
//
// A record is valid iff magic, crc, lap and epoch match *and* lsn == previous + 1.
// Recovery stops at the first invalid record: that is the durable end of the log.

pub const REC_MAGIC: u16 = 0xC0A6;
pub const REC_HDR: usize = 32;
pub const KIND_DATA: u8 = 1;
pub const KIND_PAD: u8 = 2;
pub const KIND_TX_DECISION: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecHeader {
    pub kind: u8,
    pub flags: u8,
    pub len: u32,
    pub lsn: u64,
    pub tenant: u32,
    pub lap: u32,
    pub epoch: u32,
}

#[inline]
pub const fn record_size(payload_len: usize) -> usize {
    (REC_HDR + payload_len).next_multiple_of(8)
}

/// Encode a record into `out` (must have `record_size(payload.len())` bytes).
pub fn encode_record(out: &mut [u8], h: &RecHeader, payload: &[u8]) -> usize {
    let total = record_size(payload.len());
    assert!(out.len() >= total && payload.len() == h.len as usize);
    out[0..2].copy_from_slice(&REC_MAGIC.to_le_bytes());
    out[2] = h.kind;
    out[3] = h.flags;
    out[4..8].copy_from_slice(&h.len.to_le_bytes());
    out[8..16].copy_from_slice(&h.lsn.to_le_bytes());
    out[16..20].copy_from_slice(&h.tenant.to_le_bytes());
    out[20..24].copy_from_slice(&h.lap.to_le_bytes());
    out[24..28].copy_from_slice(&h.epoch.to_le_bytes());
    out[REC_HDR..REC_HDR + payload.len()].copy_from_slice(payload);
    out[REC_HDR + payload.len()..total].fill(0);
    let crc = crc32c(&[&out[0..28], payload]);
    out[28..32].copy_from_slice(&crc.to_le_bytes());
    total
}

/// Encode a PAD record of total size `total` (multiple of 8, >= REC_HDR) in place:
/// header + zero body, no temporary allocation.
pub fn encode_pad_in_place(out: &mut [u8], h: &RecHeader, total: usize) {
    assert!(total >= REC_HDR && total % 8 == 0 && out.len() >= total);
    assert_eq!(h.len as usize, total - REC_HDR);
    assert_eq!(h.kind, KIND_PAD);
    out[0..2].copy_from_slice(&REC_MAGIC.to_le_bytes());
    out[2] = h.kind;
    out[3] = h.flags;
    out[4..8].copy_from_slice(&h.len.to_le_bytes());
    out[8..16].copy_from_slice(&h.lsn.to_le_bytes());
    out[16..20].copy_from_slice(&h.tenant.to_le_bytes());
    out[20..24].copy_from_slice(&h.lap.to_le_bytes());
    out[24..28].copy_from_slice(&h.epoch.to_le_bytes());
    out[REC_HDR..total].fill(0);
    let crc = crc32c(&[&out[0..28], &out[REC_HDR..total]]);
    out[28..32].copy_from_slice(&crc.to_le_bytes());
}

/// Decode a record at the start of `buf`. `None` = not a valid record.
pub fn decode_record(buf: &[u8]) -> Option<(RecHeader, &[u8])> {
    if buf.len() < REC_HDR || u16::from_le_bytes([buf[0], buf[1]]) != REC_MAGIC {
        return None;
    }
    let len = u32::from_le_bytes(buf[4..8].try_into().unwrap());
    let total = record_size(len as usize);
    if total > buf.len() {
        return None;
    }
    let payload = &buf[REC_HDR..REC_HDR + len as usize];
    let crc = u32::from_le_bytes(buf[28..32].try_into().unwrap());
    if crc32c(&[&buf[0..28], payload]) != crc {
        return None;
    }
    let h = RecHeader {
        kind: buf[2],
        flags: buf[3],
        len,
        lsn: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
        tenant: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
        lap: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
        epoch: u32::from_le_bytes(buf[24..28].try_into().unwrap()),
    };
    Some((h, payload))
}

// ---------------------------------------------------------------------------------
// Extent image header (payload words 0..8 of every slot / on-disk extent copy)
// ---------------------------------------------------------------------------------
//
// word 0: magic 0xC0A6_E1E7 (u32) | format version (u16) | flags (u16)
// word 1: tenant (u32) | layout version (u32)
// word 2: checkpoint_lsn  (all records <= this are folded into the image)
// word 3: log epoch at checkpoint
// word 4: image sequence (+1 per checkpoint of this tenant; picks newest of A/B)
// word 5: payload crc32c over bytes [64, extent_bytes)
// word 6: header crc32c over words 0..6 (low 32 bits)
// word 7: reserved (0)

pub const IMG_MAGIC: u32 = 0xC0A6_E1E7;
pub const IMG_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtentImage {
    pub tenant: u32,
    pub layout_version: u32,
    pub checkpoint_lsn: u64,
    pub epoch: u64,
    pub image_seq: u64,
}

impl ExtentImage {
    /// Build the 8 header words for a payload whose bytes from 64 onwards are `body`.
    pub fn header_words(&self, body: &[u8]) -> [u64; 8] {
        let mut w = [0u64; 8];
        w[0] = IMG_MAGIC as u64 | (IMG_VERSION as u64) << 32;
        w[1] = self.tenant as u64 | (self.layout_version as u64) << 32;
        w[2] = self.checkpoint_lsn;
        w[3] = self.epoch;
        w[4] = self.image_seq;
        w[5] = crc32c(&[body]) as u64;
        let mut hb = [0u8; 48];
        for (i, x) in w[..6].iter().enumerate() {
            hb[i * 8..i * 8 + 8].copy_from_slice(&x.to_le_bytes());
        }
        w[6] = crc32c(&[&hb]) as u64;
        w
    }

    /// Validate a full image (`bytes` = whole extent). Returns the decoded header.
    pub fn validate(bytes: &[u8], expect_tenant: u32) -> Option<ExtentImage> {
        if bytes.len() < 64 {
            return None;
        }
        let w: [u64; 8] = core::array::from_fn(|i| u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap()));
        if w[0] as u32 != IMG_MAGIC || (w[0] >> 32) as u16 != IMG_VERSION || w[1] as u32 != expect_tenant {
            return None;
        }
        if crc32c(&[&bytes[..48]]) as u64 != w[6] || crc32c(&[&bytes[64..]]) as u64 != w[5] {
            return None;
        }
        Some(ExtentImage { tenant: w[1] as u32, layout_version: (w[1] >> 32) as u32, checkpoint_lsn: w[2], epoch: w[3], image_seq: w[4] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_known_answer_hw_and_sw_agree() {
        // RFC 3720 B.4: 32 bytes of zeros -> 0x8A9136AA
        assert_eq!(crc32c(&[&[0u8; 32]]), 0x8A91_36AA);
        assert_eq!(crc32c(&[b"123456789"]), 0xE306_9283);
        let data: Vec<u8> = (0..10_007u32).map(|i| (i * 31 % 251) as u8).collect();
        assert_eq!(crc32c(&[&data]), !crc_sw(!0, &data));
        assert_eq!(crc32c(&[&data[..3], &data[3..]]), crc32c(&[&data]));
    }

    #[test]
    fn record_round_trip_and_corruption() {
        let h = RecHeader { kind: KIND_DATA, flags: 0, len: 5, lsn: 9, tenant: 4, lap: 0, epoch: 1 };
        let mut buf = [0u8; 64];
        let n = encode_record(&mut buf, &h, b"hello");
        assert_eq!(n, 40);
        let (d, p) = decode_record(&buf).unwrap();
        assert_eq!((d, p), (h, &b"hello"[..]));
        buf[33] ^= 1;
        assert!(decode_record(&buf).is_none());
    }

    #[test]
    fn extent_image_validation() {
        let mut ext = vec![0u8; 4096];
        ext[100] = 42;
        let img = ExtentImage { tenant: 7, layout_version: 1, checkpoint_lsn: 99, epoch: 2, image_seq: 5 };
        let hw = img.header_words(&ext[64..]);
        for (i, w) in hw.iter().enumerate() {
            ext[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        assert_eq!(ExtentImage::validate(&ext, 7), Some(img));
        assert_eq!(ExtentImage::validate(&ext, 8), None);
        ext[4000] = 1; // torn tail
        assert_eq!(ExtentImage::validate(&ext, 7), None);
    }
}
